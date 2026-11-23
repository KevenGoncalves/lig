#!/usr/bin/env python3
"""lig in nushell (issue #26), through `lig install nu`, in a private tmux server. nu comes from
$LIG_TEST_NU or PATH. Run after cargo build --release: python3 tests/nu_tmux.py."""
import os
import shutil
import subprocess
import tempfile
import time
import unittest
from pathlib import Path
from fish_pty import BIN, ROOT

NU = os.environ.get('LIG_TEST_NU') or shutil.which('nu')


@unittest.skipUnless(shutil.which('tmux') and NU, 'tmux and nu are required')
class NuTmux(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory(prefix='lig-nu-')
        directory = Path(self.dir.name)
        home, runtime = directory / 'home', directory / 'runtime'
        home.mkdir()
        runtime.mkdir(mode=0o700)
        self.log = directory / 'state/lig/lig.log'
        self.env = {'HOME': str(home), 'XDG_CONFIG_HOME': str(home / 'config'), 'XDG_DATA_HOME': str(home / 'data'),
                    'XDG_CACHE_HOME': str(home / 'cache'), 'XDG_STATE_HOME': str(directory / 'state'),
                    'XDG_RUNTIME_DIR': str(runtime), 'SHELL': NU, 'LIG_LOG': '1'}
        subprocess.run([BIN, 'install', 'nu'], env=dict(os.environ, **self.env), check=True, capture_output=True)
        # No banner, and a fixed prompt to find.
        config = home / 'config/nushell/config.nu'
        config.write_text(config.read_text().replace(
            '# <<< lig pre <<<\n',
            "# <<< lig pre <<<\n$env.config.show_banner = false\n$env.PROMPT_COMMAND = {|| 'nu-test' }\n"
            "$env.PROMPT_COMMAND_RIGHT = ''\n"))
        self.base = ['tmux', '-S', str(directory / 'socket'), '-f', '/dev/null']
        launch = ['env', '-u', 'LIG_TTY', '-u', 'LIG_DISABLE', '-u', 'INSIDE_EMACS'] + [f'{k}={v}' for k, v in self.env.items()]
        self.tmux('new-session', '-d', '-x', '100', '-y', '30', '-s', 'test', '-c', str(ROOT), *launch, NU)
        self.wait('no prompt', lambda s: 'nu-test' in s, timeout=15)

    def tearDown(self):
        subprocess.run(self.base + ['kill-server'], capture_output=True, timeout=5)
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
        self.fail(f'{what}; pane:\n{self.screen()}\nlig.log:\n{self.log.read_text() if self.log.exists() else ""}')

    def type(self, text):
        self.tmux('send-keys', '-t', 'test:0.0', '-l', text)

    def prompt_line(self):
        return next(line for line in reversed(self.screen().splitlines()) if line.startswith('nu-test'))

    def test_external_command_completes_and_inserts(self):
        self.type('git ch')
        self.wait('no git popup', lambda s: 'checkout' in s)
        self.tmux('send-keys', '-t', 'test:0.0', 'Enter')
        self.wait('checkout not inserted', lambda s: self.prompt_line().rstrip().endswith('git checkout'))
        self.assertNotIn('Error', self.screen())

    def test_nu_commands_complete_from_nu(self):
        # nu's `ls`, not coreutils': its own flags.
        self.type('ls -')
        self.wait("no nu's ls flags", lambda s: '--short-names' in s and '--du' in s)
        self.tmux('send-keys', '-t', 'test:0.0', 'Escape')
        self.tmux('send-keys', '-t', 'test:0.0', 'C-u')
        self.type('str re')
        self.wait('no str subcommands', lambda s: 'replace' in s and 'reverse' in s)

    def test_cd_runs_and_lig_follows(self):
        self.type('cd sr')
        self.wait('no folder popup', lambda s: 'src/' in s)
        self.tmux('send-keys', '-t', 'test:0.0', 'Enter')
        self.wait('src/ not inserted', lambda s: self.prompt_line().rstrip().endswith('cd src/'))
        self.tmux('send-keys', '-t', 'test:0.0', 'Enter')
        self.type('open main')
        self.wait('no file in the new folder', lambda s: 'main.rs' in s)
        # The history has the line that ran, not lig's insert key's command.
        # lig's state: XDG_STATE_HOME, or on macOS ~/Library/Application Support.
        places = [self.log.with_name('history'), Path(self.env['HOME']) / 'Library/Application Support/lig/history']
        history = lambda: next((p.read_text() for p in places if p.exists()), '')
        self.wait('cd src/ not in the history', lambda s: 'cd src/' in history())
        self.assertNotIn('__lig_insert', history())

    def test_programs_nu_runs_do_not_see_ligs_state(self):
        self.type("^env | lines | where {|l| $l =~ '^__lig' } | length | $'lig-vars=($in)'\r")
        self.wait('env did not run', lambda s: 'lig-vars=' in s.replace(self.prompt_line(), ''))
        self.assertIn('lig-vars=0', self.screen())


if __name__ == '__main__':
    unittest.main(verbosity=2)
