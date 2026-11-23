#!/usr/bin/env python3
"""Ctrl+C closes the popup with the line it abandons, also when the shell doesn't report the
next line (issue #49: a zle-line-init set after lig's), in a private tmux server.
Run after cargo build --release: python3 tests/interrupt_tmux.py."""
import os
import shutil
import subprocess
import tempfile
import time
import unittest
from pathlib import Path
from fish_pty import BIN

SHELLS = {'zsh': os.environ.get('LIG_TEST_ZSH') or shutil.which('zsh'), 'bash': shutil.which('bash'),
          'fish': shutil.which('fish')}
RC = {
    'zsh': ('.zshrc', "PROMPT='int> '\n"),
    'bash': ('.bashrc', "PS1='int> '\n"),
    'fish': ('.config/fish/config.fish', "function fish_prompt; echo -n 'int> '; end\nset -g fish_greeting\n"),
}


@unittest.skipUnless(shutil.which('tmux'), 'tmux is required')
class InterruptTmux(unittest.TestCase):
    def start(self, kind, after=''):
        shell = SHELLS[kind]
        if not shell:
            self.skipTest(f'{kind} is unavailable')
        self.dir = tempfile.TemporaryDirectory(prefix='lig-int-', ignore_cleanup_errors=True)
        directory = Path(self.dir.name)
        home, runtime = directory / 'home', directory / 'runtime'
        for i in range(20):
            (home / f'folder{i:02}').mkdir(parents=True)
        runtime.mkdir(mode=0o700)
        name, text = RC[kind]
        (home / name).parent.mkdir(parents=True, exist_ok=True)
        (home / name).write_text(text)
        # Ubuntu's /etc/zsh/zshrc runs compinit, which can stop to ask about folders' permissions.
        (home / '.zshenv').write_text('skip_global_compinit=1\n')
        self.state = directory / 'state'
        env = {'HOME': str(home), 'ZDOTDIR': str(home), 'HISTFILE': str(home / '.history'),
               'XDG_CONFIG_HOME': str(home / '.config'), 'XDG_DATA_HOME': str(home / '.local/share'),
               'XDG_CACHE_HOME': str(home / '.cache'), 'XDG_STATE_HOME': str(self.state),
               'XDG_RUNTIME_DIR': str(runtime), 'SHELL': shell, 'LIG_LOG': '1'}
        subprocess.run([BIN, 'install', kind], env=dict(os.environ, **env), check=True, capture_output=True)
        with open(home / name, 'a') as f:
            f.write(after)
        self.base = ['tmux', '-S', str(directory / 'socket'), '-f', '/dev/null']
        self.addCleanup(lambda: (subprocess.run(self.base + ['kill-server'], capture_output=True), self.dir.cleanup()))
        launch = ['env', '-u', 'LIG_TTY', '-u', 'LIG_DISABLE', '-u', 'INSIDE_EMACS'] + [f'{k}={v}' for k, v in env.items()]
        self.tmux('new-session', '-d', '-x', '100', '-y', '30', '-s', 'test', '-c', str(home), *launch, shell)
        self.wait('no prompt', lambda s: 'int>' in s, timeout=15)

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

    def check(self, kind, after=''):
        self.start(kind, after)
        popup = lambda s: 'folder00/' in s
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'ls ')
        self.wait('no folder popup', popup)
        for _ in range(3):
            self.tmux('send-keys', '-t', 'test:0.0', 'C-c')
            time.sleep(.3)
        self.wait('the popup stayed after Ctrl+C', lambda s: not popup(s))
        time.sleep(1)
        self.assertFalse(popup(self.screen()), f'the popup came back:\n{self.screen()}')
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'ls ')
        self.wait('no popup for the next line', popup)

    def test_zsh(self):
        self.check('zsh')

    def test_zsh_with_its_line_init_replaced(self):
        self.check('zsh', "zle-line-init() { : }\nzle -N zle-line-init\n")

    def test_bash(self):
        self.check('bash')

    def test_fish(self):
        self.check('fish')


if __name__ == '__main__':
    unittest.main(verbosity=2)
