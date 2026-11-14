#!/usr/bin/env python3
"""What tmux and `ps` see of the program running in a lig pane (issue #32): an editor plugin
finds a coding agent by its pane's processes and folder, which would otherwise be `lig shell`.
Run after cargo build --release: python3 tests/pane_mirror.py."""
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch
from fish_pty import BIN, Fish

ZSH = os.environ.get('LIG_TEST_ZSH') or shutil.which('zsh')


@unittest.skipUnless(shutil.which('tmux') and ZSH, 'tmux and zsh are required')
class TmuxMirror(unittest.TestCase):
    def test_pane_shows_the_foreground_program_and_its_folder(self):
        with tempfile.TemporaryDirectory(prefix='lig-mirror-') as directory:
            directory = Path(directory).resolve()
            home, project, runtime = directory / 'home', directory / 'project', directory / 'runtime'
            home.mkdir()
            project.mkdir()
            runtime.mkdir(mode=0o700)
            env = {'HOME': str(home), 'ZDOTDIR': str(home), 'SHELL': ZSH, 'XDG_CONFIG_HOME': str(home / 'config'),
                   'XDG_RUNTIME_DIR': str(runtime), 'XDG_STATE_HOME': str(directory / 'state'), 'LIG_LOG': '1'}
            (home / '.zshrc').write_text("PROMPT='lig-test> '\n")
            subprocess.run([BIN, 'install', 'zsh'], env=dict(os.environ, **env), check=True, capture_output=True)
            base = ['tmux', '-S', str(directory / 'socket'), '-f', '/dev/null']

            def tmux(*args):
                return subprocess.run(base + list(args), capture_output=True, text=True, timeout=5, check=True).stdout

            def pane(field):
                return tmux('display-message', '-p', '-t', 'test:0.0', f'#{{{field}}}').strip()

            def wait(what, check, timeout=5):
                deadline = time.monotonic() + timeout
                while time.monotonic() < deadline:
                    value = check()
                    if value:
                        return value
                    time.sleep(.1)
                log = directory / 'state/lig/lig.log'
                screen = tmux('capture-pane', '-p', '-t', 'test:0.0').rstrip()
                self.fail(f'{what}; pane:\n{screen}\nps -t: {tty_args()}\nlig.log:\n{log.read_text() if log.exists() else ""}')

            def tty_args():
                out = subprocess.run(['ps', '-t', pane('pane_tty').removeprefix('/dev/'), '-o', 'args='],
                                     capture_output=True, text=True).stdout
                return [line.split() for line in out.splitlines()]

            launch = ['env', '-u', 'LIG_TTY', '-u', 'LIG_DISABLE', '-u', 'INSIDE_EMACS', '-u', 'ZELLIJ', '-u', 'LIG_NO_MIRROR']
            launch += [f'{k}={v}' for k, v in env.items()] + [ZSH, '-i']
            try:
                tmux('new-session', '-d', '-x', '100', '-y', '30', '-s', 'test', '-c', str(home), *launch)
                # A busy CI runner can take a while to start the first shell.
                wait('no prompt', lambda: 'lig-test' in tmux('capture-pane', '-p', '-t', 'test:0.0'), timeout=15)
                # One write: with Enter sent apart, the popup could open first and take it.
                tmux('send-keys', '-t', 'test:0.0', '-l', f'cd {project} && sleep 30\r')
                wait(f'`ps -t` shows no sleep: {tty_args()}', lambda: ['sleep', '30'] in tty_args())
                wait(f'pane_current_path is {pane("pane_current_path")}', lambda: pane('pane_current_path') == str(project))
                # tmux names a macOS pane by its process name, which lig can't change.
                if sys.platform.startswith('linux'):
                    self.assertEqual(pane('pane_current_command'), 'sleep')
                tmux('send-keys', '-t', 'test:0.0', 'C-c')
                wait(f'`ps -t` still shows sleep: {tty_args()}', lambda: ['sleep', '30'] not in tty_args())
            finally:
                subprocess.run(base + ['kill-server'], capture_output=True, timeout=5)


def ps_args(pid):
    return subprocess.run(['ps', '-o', 'args=', '-p', str(pid)], capture_output=True, text=True).stdout.split()


def cwd(pid):
    if sys.platform.startswith('linux'):
        return os.readlink(f'/proc/{pid}/cwd')
    out = subprocess.run(['lsof', '-a', '-p', str(pid), '-d', 'cwd', '-Fn'], capture_output=True, text=True).stdout
    return next((line[1:] for line in out.splitlines() if line.startswith('n')), None)


@unittest.skipUnless(ZSH, 'zsh is required')
class PsMirror(unittest.TestCase):
    def setUp(self):
        self.env = patch.dict(os.environ)
        self.env.start()
        for key in ['ZELLIJ', 'LIG_NO_MIRROR', 'LIG_MIRROR_PANE']:
            os.environ.pop(key, None)
        self.folder = tempfile.TemporaryDirectory(prefix='lig-mirror-')
        self.term = None

    def tearDown(self):
        try:
            if self.term:
                self.term.close()
        finally:
            self.env.stop()
            self.folder.cleanup()

    def wait(self, what, check):
        deadline = time.monotonic() + 6
        while time.monotonic() < deadline:
            if check():
                return
            self.term.read(.1)
        self.fail(f'{what}: ps shows {ps_args(self.term.pid)}, cwd {cwd(self.term.pid)}')

    def test_ps_shows_the_program_and_its_folder_through_a_nested_lig(self):
        folder = str(Path(self.folder.name).resolve())
        # zsh's pre-hook execs lig, so this pid is lig.
        self.term = Fish(wrapped=True, shell=ZSH)
        pid = self.term.pid
        self.term.send(f'cd {folder} && sleep 30\r')
        self.wait('sleep 30 in its folder', lambda: ps_args(pid) == ['sleep', '30'] and cwd(pid) == folder)
        self.term.send(b'\x03')
        self.wait('back to the shell', lambda: ps_args(pid) and ps_args(pid)[0].endswith('zsh'))
        # A lig started in this one shows what runs in it, and this one passes that on.
        self.term.send(f'{BIN} shell --shell {ZSH}\r')
        self.term.read(1.5)
        self.term.send('sleep 31\r')
        self.wait('sleep 31 from the nested lig', lambda: ps_args(pid) == ['sleep', '31'])
        self.term.send(b'\x03')
        self.term.send('exit\r')


if __name__ == '__main__':
    unittest.main(verbosity=2)
