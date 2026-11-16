#!/usr/bin/env python3
"""The shell's own completions fill in for commands Lig has no spec for (issue #38:
behavior.completers = ["shell"]), in zsh and fish, in a private tmux server.
Run after cargo build --release: python3 tests/completers_tmux.py."""
import os
import shutil
import subprocess
import tempfile
import time
import unittest
from pathlib import Path
from fish_pty import BIN

SHELLS = {'zsh': os.environ.get('LIG_TEST_ZSH') or shutil.which('zsh'), 'fish': shutil.which('fish')}
RC = {
    'zsh': ('.zshrc', "PROMPT='comp> '\nautoload -Uz compinit && compinit -u -d $HOME/.zcompdump\n"),
    'fish': ('.config/fish/config.fish', "function fish_prompt; echo -n 'comp> '; end\nset -g fish_greeting\n"),
}


@unittest.skipUnless(shutil.which('tmux'), 'tmux is required')
class CompletersTmux(unittest.TestCase):
    def start(self, kind, completers='["shell"]'):
        shell = SHELLS[kind]
        if not shell:
            self.skipTest(f'{kind} is unavailable')
        self.dir = tempfile.TemporaryDirectory(prefix='lig-comp-', ignore_cleanup_errors=True)
        directory = Path(self.dir.name)
        home, runtime = directory / 'home', directory / 'runtime'
        (home / '.config/lig').mkdir(parents=True)
        runtime.mkdir(mode=0o700)
        name, text = RC[kind]
        (home / name).parent.mkdir(parents=True, exist_ok=True)
        (home / name).write_text(text)
        # Ubuntu's /etc/zsh/zshrc runs compinit, which can stop to ask about folders' permissions.
        (home / '.zshenv').write_text('skip_global_compinit=1\n')
        (home / '.config/lig/config.toml').write_text(f'[behavior]\ncompleters = {completers}\nhelp_parsing = false\n')
        self.state = directory / 'state'
        env = {'HOME': str(home), 'ZDOTDIR': str(home), 'XDG_CONFIG_HOME': str(home / '.config'),
               'XDG_DATA_HOME': str(home / '.local/share'), 'XDG_CACHE_HOME': str(home / '.cache'),
               'XDG_STATE_HOME': str(self.state), 'XDG_RUNTIME_DIR': str(runtime), 'SHELL': shell, 'LIG_LOG': '1'}
        subprocess.run([BIN, 'install', kind], env=dict(os.environ, **env), check=True, capture_output=True)
        self.base = ['tmux', '-S', str(directory / 'socket'), '-f', '/dev/null']
        self.addCleanup(lambda: (subprocess.run(self.base + ['kill-server'], capture_output=True), self.dir.cleanup()))
        launch = ['env', '-u', 'LIG_TTY', '-u', 'LIG_DISABLE', '-u', 'INSIDE_EMACS'] + [f'{k}={v}' for k, v in env.items()]
        self.tmux('new-session', '-d', '-x', '100', '-y', '30', '-s', 'test', '-c', str(home), *launch, shell)
        self.wait('no prompt', lambda s: 'comp>' in s, timeout=15)

    def tmux(self, *args):
        return subprocess.run(self.base + list(args), capture_output=True, text=True, timeout=5, check=True).stdout

    def screen(self):
        return self.tmux('capture-pane', '-p', '-t', 'test:0.0')

    def wait(self, what, check, timeout=8):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if check(self.screen()):
                return
            time.sleep(.1)
        log = self.state / 'lig/lig.log'
        self.fail(f'{what}; pane:\n{self.screen()}\nlig.log:\n{log.read_text()[-2000:] if log.exists() else ""}')

    def popup(self, s):
        return ''.join(l for l in s.splitlines() if '│' in l)

    def prompt(self, s):
        return [l for l in s.splitlines() if l.startswith('comp>')][-1].rstrip()

    def test_zsh_offers_what_its_tab_would(self):
        self.start('zsh')
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'setopt auto_c')
        self.wait("no setopt options from zsh", lambda s: 'auto_cd' in self.popup(s) and 'auto_continue' in self.popup(s))
        self.assertEqual(self.prompt(self.screen()), 'comp> setopt auto_c', 'asking zsh changed the line')
        self.tmux('send-keys', '-t', 'test:0.0', 'Enter')
        self.wait('auto_cd not inserted', lambda s: self.prompt(s) == 'comp> setopt auto_cd')

    def test_fish_offers_what_its_tab_would(self):
        self.start('fish')
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'string esc')
        self.wait("no string subcommands from fish", lambda s: 'escape' in self.popup(s))
        self.tmux('send-keys', '-t', 'test:0.0', 'Enter')
        self.wait('escape not inserted', lambda s: self.prompt(s) == 'comp> string escape')

    def test_off_by_default(self):
        self.start('zsh', completers='[]')
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'setopt auto_c')
        time.sleep(2)
        self.assertNotIn('auto_cd', self.popup(self.screen()))


if __name__ == '__main__':
    unittest.main(verbosity=2)
