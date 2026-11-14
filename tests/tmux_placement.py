#!/usr/bin/env python3
"""Popup placement and restoration in a private tmux server."""
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from fish_pty import BIN, FISH, ROOT


@unittest.skipUnless(shutil.which('tmux') and FISH, 'tmux and fish are required')
class TmuxPlacement(unittest.TestCase):
    def test_existing_pane_cursor(self):
        self.check_pane(8)

    def test_bottom_of_pane_preserves_output(self):
        self.check_pane(35)

    def test_tmux_256color(self):
        self.check_pane(8, terminal='tmux-256color', width=158, height=48)

    def test_bash_preserves_output(self):
        shell = shutil.which('bash')
        if not shell:
            self.skipTest('bash is unavailable')
        self.check_pane(35, shell=shell)

    def test_zsh_preserves_output(self):
        shell = os.environ.get('LIG_TEST_ZSH') or shutil.which('zsh')
        if not shell:
            self.skipTest('zsh is unavailable')
        self.check_pane(35, shell=shell)

    # A split opening and closing resizes the pane: tmux scrolls its content to keep the cursor
    # in view, and lig's screen model must follow (issue #25).
    def test_split_and_close_preserve_output(self):
        self.check_pane(35, split=True)

    def test_bash_split_and_close_preserve_output(self):
        shell = shutil.which('bash')
        if not shell:
            self.skipTest('bash is unavailable')
        self.check_pane(35, shell=shell, split=True)

    def check_pane(self, lines, terminal='screen', width=100, height=30, shell=None, split=False):
        with tempfile.TemporaryDirectory(prefix='lig-tmux-') as directory:
            directory = Path(directory)
            config = directory / 'tmux.conf'
            config.write_text(f'set -g default-terminal {terminal}\nset -g allow-passthrough on\n')
            base = ['tmux', '-S', str(directory / 'socket'), '-f', str(config)]

            def tmux(*args):
                result = subprocess.run(base + list(args), capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, 0, result.stderr)
                return result.stdout

            def capture(history=False):
                return tmux('capture-pane', '-p', '-t', 'test:0.0', *(['-S', '-'] if history else []))

            def wait_for(text):
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    output = capture()
                    if text in output:
                        return output
                    time.sleep(.02)
                log = directory / 'state/lig/lig.log'
                detail = log.read_text() if log.exists() else ''
                self.fail(f'{text!r} missing from pane:\n{output}\n{detail}')

            if shell is None:
                launch = [sys.executable, str(ROOT / 'tools/try-fish.py'), '--bin', str(BIN)]
            else:
                home = directory / 'home'
                home.mkdir()
                runtime = directory / 'runtime'
                runtime.mkdir(mode=0o700)
                env = dict(os.environ, HOME=str(home), ZDOTDIR=str(home), SHELL=shell,
                           XDG_CONFIG_HOME=str(home / 'config'), XDG_RUNTIME_DIR=str(runtime),
                           XDG_STATE_HOME=str(directory / 'state'), LIG_LOG='1')
                for key in ['LIG_TTY', 'LIG_DISABLE', 'INSIDE_EMACS']:
                    env.pop(key, None)
                kind = Path(shell).name
                rc = home / ('.bashrc' if kind == 'bash' else '.zshrc')
                text = "PS1='lig-test> '\n" if kind == 'bash' else "zmodload zsh/zle\nPROMPT='lig-test> '\n"
                if kind == 'zsh' and os.environ.get('LIG_TEST_ZSH_ROOT'):
                    root = shlex.quote(os.environ['LIG_TEST_ZSH_ROOT'])
                    text = (f'module_path=({root}/usr/lib/x86_64-linux-gnu/zsh/5.9 $module_path)\n'
                            f'fpath=({root}/usr/share/zsh/functions/**/*(/) $fpath)\n' + text)
                rc.write_text(text)
                subprocess.run([BIN, 'install', kind], env=env, check=True, capture_output=True)
                launch = ['env', '-u', 'LIG_TTY', '-u', 'LIG_DISABLE', '-u', 'INSIDE_EMACS']
                launch += [f'{key}={env[key]}' for key in ['HOME', 'ZDOTDIR', 'SHELL', 'XDG_CONFIG_HOME', 'XDG_RUNTIME_DIR', 'XDG_STATE_HOME', 'LIG_LOG']]
                launch += [shell, '-i']
            command = f'for n in $(seq 1 {lines}); do printf "before-%s\\n" "$n"; done; exec ' + shlex.join(launch)
            try:
                tmux('new-session', '-d', '-x', str(width), '-y', str(height), '-s', 'test', 'sh', '-c', command)
                wait_for('lig-test')
                before = capture(history=True)
                tmux('send-keys', '-t', 'test:0.0', '-l', 'git ch')
                popup = wait_for('checkout').splitlines()
                prompt_row = next(i for i, line in enumerate(popup) if line.startswith('lig-test'))
                menu_row = next(i for i, line in enumerate(popup) if 'checkout' in line)
                self.assertEqual(menu_row, prompt_row + 2, '\n'.join(popup))
                if split:
                    tmux('split-window', '-v', '-t', 'test:0.0', 'sleep 30')
                    time.sleep(.5)
                    tmux('kill-pane', '-t', 'test:0.1')
                    wait_for('checkout')
                    time.sleep(.5)
                tmux('send-keys', '-t', 'test:0.0', 'Escape')
                deadline = time.monotonic() + 3
                while 'checkout' in capture() and time.monotonic() < deadline:
                    time.sleep(.02)
                self.assertNotIn('checkout', capture(), 'the popup stayed on screen')
                after = capture(history=True)
                def prior_output(text):
                    return [line for line in text.splitlines() if line and not line.startswith('lig-test')]
                self.assertEqual(prior_output(before), prior_output(after))
                tmux('send-keys', '-t', 'test:0.0', 'C-l')
                tmux('send-keys', '-t', 'test:0.0', '-l', 'e')
                wait_for('checkout')
            finally:
                subprocess.run(base + ['kill-server'], capture_output=True, timeout=5)


if __name__ == '__main__':
    unittest.main(verbosity=2)
