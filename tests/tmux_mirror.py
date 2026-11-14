#!/usr/bin/env python3
"""What tmux and `ps -t` see of the program running in a lig pane (issue #32): an editor plugin
finds a coding agent by its pane's processes and folder, which would otherwise be `lig shell`."""
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from fish_pty import BIN

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

            def wait(what, check):
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    value = check()
                    if value:
                        return value
                    time.sleep(.1)
                log = directory / 'state/lig/lig.log'
                self.fail(f'{what}; lig.log:\n{log.read_text() if log.exists() else ""}')

            def tty_args():
                out = subprocess.run(['ps', '-t', pane('pane_tty').removeprefix('/dev/'), '-o', 'args='],
                                     capture_output=True, text=True).stdout
                return [line.split() for line in out.splitlines()]

            launch = ['env', '-u', 'LIG_TTY', '-u', 'LIG_DISABLE', '-u', 'INSIDE_EMACS', '-u', 'ZELLIJ', '-u', 'LIG_NO_MIRROR']
            launch += [f'{k}={v}' for k, v in env.items()] + [ZSH, '-i']
            try:
                tmux('new-session', '-d', '-x', '100', '-y', '30', '-s', 'test', '-c', str(home), *launch)
                wait('no prompt', lambda: 'lig-test' in tmux('capture-pane', '-p', '-t', 'test:0.0'))
                tmux('send-keys', '-t', 'test:0.0', '-l', f'cd {project} && sleep 30')
                tmux('send-keys', '-t', 'test:0.0', 'Enter')
                wait(f'`ps -t` shows no sleep: {tty_args()}', lambda: ['sleep', '30'] in tty_args())
                wait(f'pane_current_path is {pane("pane_current_path")}', lambda: pane('pane_current_path') == str(project))
                # tmux names a macOS pane by its process name, which lig can't change.
                if sys.platform.startswith('linux'):
                    self.assertEqual(pane('pane_current_command'), 'sleep')
                tmux('send-keys', '-t', 'test:0.0', 'C-c')
                wait(f'`ps -t` still shows sleep: {tty_args()}', lambda: ['sleep', '30'] not in tty_args())
            finally:
                subprocess.run(base + ['kill-server'], capture_output=True, timeout=5)


if __name__ == '__main__':
    unittest.main(verbosity=2)
