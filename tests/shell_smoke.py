#!/usr/bin/env python3
"""Bash/zsh PTY regression smoke tests; LIG_TEST_ZSH can name an extracted zsh."""
import os
import re
import shutil
import unittest
from pathlib import Path
from fish_pty import Fish

class ShellSmoke(unittest.TestCase):
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

    def enter_runs(self, shell, extra=''):
        """`enter_runs_commands`: one Enter picks the suggestion and runs the line."""
        spec = '{"name": "jump", "args": {"name": "place", "suggestions": ["alpha", "beta"]}}'
        term = Fish(wrapped=True, shell=shell, extra_config=extra, after_config='jump() { echo "JUMPED:$1"; }\n',
                    lig_config='[behavior]\nenter_runs_commands = ["jump"]\n', specs={'jump': spec})
        try:
            # Mid-word: the match is picked and run.
            start = len(term.output)
            term.send('jump al')
            term.wait_popup('alpha', start)
            start = len(term.output)
            term.send(b'\r')
            term.wait_text('JUMPED:alpha', start)
            self.assertEqual(term.buffer(), '')
            # A selection moved with Tab is the one that runs.
            start = len(term.output)
            term.send('jump ')
            term.wait_popup('beta', start)
            term.send(b'\t')
            start = len(term.output)
            term.send(b'\r')
            term.wait_text('JUMPED:beta', start)
            self.assertEqual(term.buffer(), '')
            # The command is the one the cursor is in, not the first on the line.
            start = len(term.output)
            term.send('true && jump al')
            term.wait_popup('alpha', start)
            start = len(term.output)
            term.send(b'\r')
            term.wait_text('JUMPED:alpha', start)
            self.assertEqual(term.buffer(), '')
        finally:
            self.assertEqual(term.close(17), 17)

    def test_bash(self):
        shell = shutil.which('bash')
        if not shell:
            self.skipTest('bash is unavailable')
        self.smoke(shell)
        self.enter_runs(shell)

    def test_zsh(self):
        shell = os.environ.get('LIG_TEST_ZSH') or shutil.which('zsh')
        if not shell:
            self.skipTest('zsh is not available')
        extra = ''
        if os.environ.get('LIG_TEST_ZSH_ROOT'):
            root = Path(os.environ['LIG_TEST_ZSH_ROOT'])
            extra = f"module_path=({root}/usr/lib/x86_64-linux-gnu/zsh/5.9 $module_path)\nfpath=({root}/usr/share/zsh/functions/**/*(/) $fpath)\n"
        self.smoke(shell, extra)
        self.enter_runs(shell, extra)

if __name__ == '__main__':
    unittest.main(verbosity=2)
