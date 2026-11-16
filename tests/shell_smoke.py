#!/usr/bin/env python3
"""Bash/zsh PTY regression smoke tests; LIG_TEST_ZSH can name an extracted zsh."""
import os
import re
import shutil
import unittest
from pathlib import Path
from fish_pty import Fish

class ShellSmoke(unittest.TestCase):
    def test_bash_completed_operands_reach_commands(self):
        shell = shutil.which('bash')
        if not shell:
            self.skipTest('bash is unavailable')
        specs = {
            name: f'{{"name":"{name}","args":{{"suggestions":["{operand}"]}}}}'
            for name, operand in [('mkdir', 'created'), ('touch', 'created.txt'), ('cd', 'destination')]
        }
        term = Fish(wrapped=True, shell=shell, specs=specs,
                    after_config='mkdir -p "$HOME/destination"\ncd "$HOME"\n')
        try:
            for command, prefix, operand in [('mkdir', 'cre', 'created'),
                                             ('touch', 'cre', 'created.txt'),
                                             ('cd', 'des', 'destination')]:
                start = len(term.output)
                term.send(f'{command} {prefix}')
                term.wait_popup(operand, start)
                term.send(b'\r')
                term.send(b'\r')
                if command == 'cd':
                    term.send('pwd > "$HOME/cwd"\r')
                    term.wait_for(lambda: (term.home / 'cwd').exists())
                    self.assertEqual((term.home / 'cwd').read_text().strip(), str(term.home / operand))
                else:
                    self.assertTrue((term.home / operand).exists(), term.plain(start))
        finally:
            self.assertEqual(term.close(17), 17)

    def test_theme_reloads_with_the_popup_open(self):
        """A new theme in config.toml shows in a running session; a broken file keeps the last one."""
        shell = shutil.which('bash')
        if not shell:
            self.skipTest('bash is unavailable')

        def palette(color):
            return f'[colors]\nselected_bg = "{color}"\n'

        term = Fish(wrapped=True, shell=shell, lig_config=palette('#010203'))
        config = term.home / 'config/lig/config.toml'

        def write(text):
            # Replaced whole, as theme switchers do.
            new = config.with_suffix('.new')
            new.write_text(text)
            new.replace(config)

        try:
            start = len(term.output)
            term.send('git ch')
            term.wait_popup('checkout', start)
            term.wait_for(lambda: b'48;2;1;2;3' in term.output[start:])
            # With the popup open, no key needed.
            start = len(term.output)
            write(palette('#040506'))
            term.wait_for(lambda: b'48;2;4;5;6' in term.output[start:])
            # A broken file keeps the theme in use.
            write('[colors\n')
            term.wait_for(lambda: 'keeping the theme' in term.log())
            start = len(term.output)
            term.send('e')
            term.wait_popup('checkout', start)
            self.assertIn(b'48;2;4;5;6', term.output[start:])
            start = len(term.output)
            write(palette('#010203'))
            term.wait_for(lambda: b'48;2;1;2;3' in term.output[start:])
        finally:
            self.assertEqual(term.close(17), 17)

    def test_zsh_hook_widget_wrapped_by_another_plugin(self):
        """zsh-history-substring-search hooks zle-line-finish, then fast-syntax-highlighting wraps the
        hook's dispatcher: lig must not hook it again (issue #29: endless recursion, FUNCNEST)."""
        shell = os.environ.get('LIG_TEST_ZSH') or shutil.which('zsh')
        if not shell:
            self.skipTest('zsh is unavailable')
        plugins = (
            'FUNCNEST=100\nautoload -Uz add-zle-hook-widget\nzmodload zsh/zleparameter\n'
            '_test_hook() { : }\nadd-zle-hook-widget line-finish _test_hook\n'
            "zle -N orig-zle-line-finish ${widgets[zle-line-finish]#user:}\n"
            '_test_wrap() { zle orig-zle-line-finish -- "$@" }\nzle -N zle-line-finish _test_wrap\n'
        )
        term = Fish(wrapped=True, shell=shell, extra_config=plugins)
        try:
            start = len(term.output)
            term.send('echo hook-ran\r')
            term.wait_for(lambda: term.plain(start).count(b'hook-ran') >= 2)
            self.assertNotIn(b'nested function level', term.plain(start))
            start = len(term.output)
            term.send('git ch')
            term.wait_popup('checkout', start)
        finally:
            self.assertEqual(term.close(17), 17)

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
