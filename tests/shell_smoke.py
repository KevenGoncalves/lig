#!/usr/bin/env python3
"""Bash/zsh PTY regression smoke tests; LIG_TEST_ZSH can name an extracted zsh."""
import os
import re
import shutil
import time
import unittest
from pathlib import Path
from fish_pty import Fish

class ShellSmoke(unittest.TestCase):
    def test_reload_theme_with_popup_open(self):
        shell = shutil.which('bash')
        if not shell:
            self.skipTest('bash is unavailable')
        term = Fish(wrapped=True, shell=shell, initial_config='[colors]\nselected_bg = "#010203"\n')
        try:
            config = term.home / 'config/lig/config.toml'

            def write(text):
                temporary = config.with_suffix('.new')
                temporary.write_text(text)
                temporary.replace(config)

            def palette(color):
                return f'[colors]\nselected_bg = "{color}"\n'

            start = len(term.output)
            term.send('git ch')
            term.wait_popup('checkout', start)
            term.wait_for(lambda: b'48;2;1;2;3' in term.output[start:])

            start = len(term.output)
            write(palette('#040506'))
            term.wait_for(lambda: b'48;2;4;5;6' in term.output[start:], timeout=3)

            start = len(term.output)
            write('[colors\n')
            time.sleep(.6)
            term.send('e')
            term.wait_for(lambda: b'48;2;4;5;6' in term.output[start:])

            start = len(term.output)
            write(palette('#010203'))
            term.wait_for(lambda: b'48;2;1;2;3' in term.output[start:], timeout=3)
        finally:
            self.assertEqual(term.close(), 0)

    def smoke(self, shell, extra=''):
        term = Fish(wrapped=True, shell=shell, extra_config=extra)
        try:
            start = len(term.output)
            term.send('git ch')
            output = term.wait_text('checkout', start)
            plain = re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]', b'', output)
            self.assertIn(b'checkout', plain)
            term.send(b'\t')
            term.send(b'\r')
            self.assertEqual(term.buffer(), 'git cherry-pick')
            term.send(b'\x03')
            self.assertIn(b'REGRESSION_OK', term.send(b'echo REGRESSION_OK\r'))
        finally:
            self.assertEqual(term.close(17), 17)

    def test_bash(self):
        shell = shutil.which('bash')
        if not shell:
            self.skipTest('bash is unavailable')
        self.smoke(shell)

    def test_zsh(self):
        shell = os.environ.get('LIG_TEST_ZSH') or shutil.which('zsh')
        if not shell:
            self.skipTest('zsh is not available')
        extra = ''
        if os.environ.get('LIG_TEST_ZSH_ROOT'):
            root = Path(os.environ['LIG_TEST_ZSH_ROOT'])
            extra = f"module_path=({root}/usr/lib/x86_64-linux-gnu/zsh/5.9 $module_path)\nfpath=({root}/usr/share/zsh/functions/**/*(/) $fpath)\n"
        self.smoke(shell, extra)

if __name__ == '__main__':
    unittest.main(verbosity=2)
