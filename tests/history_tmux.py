#!/usr/bin/env python3
"""Ctrl+R shows the commands that ran (issue #27; with history_success_only, only those that
ran without error, issue #56), in zsh, bash, fish and nu,
in a private tmux server. Run after cargo build --release: python3 tests/history_tmux.py."""
import os
import re
import shutil
import sys
import subprocess
import tempfile
import time
import unittest
from pathlib import Path
from fish_pty import BIN

SHELLS = {
    'zsh': os.environ.get('LIG_TEST_ZSH') or shutil.which('zsh'),
    'bash': shutil.which('bash'),
    'fish': shutil.which('fish'),
    'nu': os.environ.get('LIG_TEST_NU') or shutil.which('nu'),
}
PROMPTS = {
    'zsh': "PROMPT='hist-test> '\n",
    # A hook before lig's that leaves $? at 0: with history_success_only, the failed command must still stay out.
    'bash': "PS1='hist-test> '\nPROMPT_COMMAND='true'\n",
    'fish': "function fish_prompt; echo -n 'hist-test> '; end\nset -g fish_greeting\n",
    'nu': "$env.config.show_banner = false\n$env.PROMPT_COMMAND = {|| 'hist-test' }\n$env.PROMPT_COMMAND_RIGHT = ''\n",
}


@unittest.skipUnless(shutil.which('tmux'), 'tmux is required')
class HistoryTmux(unittest.TestCase):
    def start(self, kind, config='', rc_extra='', custom_history=None, lig_history=None):
        shell = SHELLS[kind]
        if not shell:
            self.skipTest(f'{kind} is unavailable')
        self.dir = tempfile.TemporaryDirectory(prefix='lig-hist-')
        directory = Path(self.dir.name)
        home, runtime = directory / 'home', directory / 'runtime'
        home.mkdir()
        runtime.mkdir(mode=0o700)
        (home / '.zsh_history').write_text(': 1700000000:0;echo imported-from-zsh\n')
        # Ubuntu's /etc/zsh/zshrc runs compinit, which can stop to ask about folders' permissions.
        (home / '.zshenv').write_text('skip_global_compinit=1\n')
        self.state = directory / 'state'
        env = {'HOME': str(home), 'ZDOTDIR': str(home), 'XDG_CONFIG_HOME': str(home / '.config'),
               'XDG_DATA_HOME': str(home / '.local/share'), 'XDG_CACHE_HOME': str(home / '.cache'),
               'XDG_STATE_HOME': str(self.state), 'XDG_RUNTIME_DIR': str(runtime), 'SHELL': shell, 'LIG_LOG': '1',
               'HISTFILE': str(home / '.shell_history')}
        rc = {'zsh': home / '.zshrc', 'bash': home / '.bashrc', 'fish': home / '.config/fish/config.fish',
              'nu': home / '.config/nushell/config.nu'}[kind]
        rc.parent.mkdir(parents=True, exist_ok=True)
        rc.write_text(PROMPTS[kind] + rc_extra)
        if custom_history is not None:
            (home / 'custom').mkdir()
            (home / 'custom/history').write_text(custom_history)
        if lig_history is not None:
            # A history lig already has: the shells' files no longer seed it.
            lig = home / 'Library/Application Support/lig' if sys.platform == 'darwin' else self.state / 'lig'
            lig.mkdir(parents=True)
            (lig / 'history').write_text(lig_history)
        if config:
            (home / '.config/lig').mkdir(parents=True, exist_ok=True)
            (home / '.config/lig/config.toml').write_text(config)
        subprocess.run([BIN, 'install', kind], env=dict(os.environ, **env), check=True, capture_output=True)
        self.base = ['tmux', '-S', str(directory / 'socket'), '-f', '/dev/null']
        self.addCleanup(self.stop)
        launch = ['env', '-u', 'LIG_TTY', '-u', 'LIG_DISABLE', '-u', 'INSIDE_EMACS'] + [f'{k}={v}' for k, v in env.items()]
        self.tmux('new-session', '-d', '-x', '100', '-y', '30', '-s', 'test', '-c', str(home), *launch, shell)
        self.wait('no prompt', lambda s: 'hist-test' in s, timeout=15)

    def stop(self):
        subprocess.run(self.base + ['kill-server'], capture_output=True)
        # bash writes its history when the hangup reaches it, which can be mid-removal.
        for _ in range(50):
            try:
                return self.dir.cleanup()
            except OSError:
                time.sleep(.1)
        self.dir.cleanup()

    def tmux(self, *args):
        return subprocess.run(self.base + list(args), capture_output=True, text=True, timeout=5, check=True).stdout

    def screen(self):
        return self.tmux('capture-pane', '-p', '-t', 'test:0.0')

    def wait(self, what, check, timeout=6):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if check(self.screen()):
                return
            time.sleep(.1)
        log = self.state / 'lig/lig.log'
        self.fail(f'{what}; pane:\n{self.screen()}\nlig.log:\n{log.read_text()[-3000:] if log.exists() else ""}')

    def run_line(self, line):
        # One write: with Enter sent apart, the popup could open first and take it.
        self.tmux('send-keys', '-t', 'test:0.0', '-l', f'{line}\r')
        self.wait(f'{line!r} did not finish', lambda s: s.rstrip().splitlines()[-1].startswith('hist-test'))

    def check_history(self, kind, success_only=False):
        self.start(kind, '[behavior]\nhistory_success_only = true\n' if success_only else '')
        self.run_line('echo hist-ok')
        self.run_line('ls /no/such/dir-hist-bad')
        self.run_line(' echo hist-hidden')
        time.sleep(.5)
        self.tmux('send-keys', '-t', 'test:0.0', 'C-r')
        self.wait('no history popup', lambda s: 'echo hist-ok' in s and 'imported-from-zsh' in s)
        entries = ''.join(l for l in self.screen().splitlines() if '↺' in l)
        if success_only:
            self.assertNotIn('dir-hist-bad', entries)
        else:
            self.assertIn('dir-hist-bad', entries)
        self.assertNotIn('hist-hidden', entries)
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'hist-o')
        self.wait('not narrowed', lambda s: 'imported-from-zsh' not in s and 'echo hist-ok' in s)
        self.tmux('send-keys', '-t', 'test:0.0', 'Enter')
        # The last prompt's line: nu may show a hint for the command under it.
        prompt = lambda s: [l for l in s.splitlines() if l.startswith('hist-test')][-1].rstrip()
        self.wait('not picked', lambda s: prompt(s).endswith('echo hist-ok') and '↺' not in s)

    def test_zsh(self):
        self.check_history('zsh')

    def test_bash(self):
        self.check_history('bash')

    def test_fish(self):
        self.check_history('fish')

    def test_nu(self):
        self.check_history('nu')

    def test_success_only_leaves_failures_out(self):
        for kind in ['zsh', 'bash', 'fish', 'nu']:
            with self.subTest(kind):
                self.check_history(kind, success_only=True)
                self.doCleanups()

    def test_folder_history_on_alt_r(self):
        for kind in ['zsh', 'bash', 'fish', 'nu']:
            with self.subTest(kind):
                # Enter always runs the line: after `cd`, it would pick from the folder popup.
                self.start(kind, '[behavior]\nenter_accepts = "never"\n')
                for line in ['mkdir fa fb', 'cd fa', 'echo in-fa', 'cd ../fb', 'echo in-fb']:
                    self.run_line(line)
                time.sleep(.5)
                entries = lambda: ''.join(l for l in self.screen().splitlines() if '↺' in l)
                self.tmux('send-keys', '-t', 'test:0.0', 'M-r')
                self.wait("no folder history", lambda s: 'echo in-fb' in entries())
                self.assertNotIn('in-fa', entries())
                self.assertIn('local', entries())
                # Ctrl+R switches to all of it.
                self.tmux('send-keys', '-t', 'test:0.0', 'C-r')
                self.wait('no full history', lambda s: 'echo in-fa' in entries() and 'echo in-fb' in entries())
                self.assertNotIn('local', entries())
                self.doCleanups()

    def test_histfile_set_in_the_rc_file_is_added(self):
        """HISTFILE set in .zshrc or .bashrc isn't exported: the hooks tell lig where it is."""
        files = {'zsh': ': 1700000000:0;echo from-custom-histfile\n',
                 'bash': '#1700000000\necho from-custom-histfile\n'}
        for kind, text in files.items():
            with self.subTest(kind):
                self.start(kind, rc_extra='HISTFILE="$HOME/custom/history"\n', custom_history=text,
                           lig_history='1700000100\techo recorded-by-lig\n')
                self.run_line('echo hist-ok')
                time.sleep(.5)
                self.tmux('send-keys', '-t', 'test:0.0', 'C-r')
                entries = lambda: ''.join(l for l in self.screen().splitlines() if '↺' in l)
                self.wait('HISTFILE not added',
                          lambda s: all(c in entries() for c in ['echo hist-ok', 'recorded-by-lig', 'from-custom-histfile']))
                # bash's `#<time>` lines date the commands; they aren't commands.
                self.assertNotIn('#1700000000', entries())
                self.doCleanups()

    def test_typed_letters_are_bold_inside_a_command_too(self):
        """Issue #63: `cl` matches `hipotclear` inside, and shows bold there as in `clear`."""
        self.start('bash', lig_history='1\thipotclear\n2\tanonclear\n3\tclear\n')
        self.tmux('send-keys', '-t', 'test:0.0', 'C-r')
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'cl')
        self.wait('no matches', lambda s: 'hipotclear' in s and 'anonclear' in s)
        sgr = r'(?:\x1b\[[0-9;]*m)*'
        styled = self.tmux('capture-pane', '-p', '-e', '-t', 'test:0.0')
        for before in ['hipot', 'anon']:
            m = re.search(re.escape(before) + f'({sgr})c{sgr}l', styled)
            self.assertTrue(m, f'{before}cl not found:\n{styled!r}')
            self.assertRegex(m.group(1), r'\x1b\[(?:[0-9;]*;)?1[;m]', f'{before}: cl not bold')

    def test_max_width_shows_a_long_command_whole(self):
        """Issue #65: with `width = "auto"`, `max_width` let only 44 columns of a command show."""
        long = 'docker compose -f docker-compose.prod.yml up -d --build --remove-orphans web'
        self.start('bash', '[ui]\nwidth = "auto"\nmax_width = 888\n', lig_history=f'1700000000\t{long}\n')
        self.tmux('send-keys', '-t', 'test:0.0', 'C-r')
        self.wait('no history popup', lambda s: 'docker compose' in s)
        self.wait('command cut', lambda s: long in s)

    def test_off_leaves_ctrl_r_to_the_shell(self):
        self.start('bash', '[behavior]\nhistory = false\n')
        self.run_line('echo hist-ok')
        self.tmux('send-keys', '-t', 'test:0.0', 'C-r')
        self.wait("bash's own search did not open", lambda s: 'reverse-i-search' in s)
        self.assertFalse((self.state / 'lig/history').exists())


if __name__ == '__main__':
    unittest.main(verbosity=2)
