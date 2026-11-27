#!/usr/bin/env python3
"""Bash/zsh PTY regression smoke tests; LIG_TEST_ZSH can name an extracted zsh."""
import fcntl
import os
import re
import shlex
import shutil
import signal
import struct
import subprocess
import sys
import termios
import time
import unittest
from pathlib import Path
from fish_pty import Fish


def zsh_cursor_query(repeats=1, split=False):
    if split:
        # The fake terminal resizes only after the prefix has crossed the PTY.
        # The child sends the tail only after Lig applies that resize.
        query = (
            ' os.write(1, b"\\x1b[6")\n'
            ' until = time.monotonic() + 5\n'
            ' while os.get_terminal_size(1).columns != 100:\n'
            '  if time.monotonic() >= until: raise RuntimeError("no resize")\n'
            '  time.sleep(.005)\n'
            ' os.write(1, b"n")\n'
        )
    else:
        query = f' os.write(1, b"\\x1b[6n" * {repeats})\n'
    program = (
        'import os, select, termios, time, tty\n'
        'old = termios.tcgetattr(0)\n'
        'tty.setraw(0)\n'
        'try:\n'
        ' os.write(1, b"CHILD_DSR_READY\\n")\n'
        f'{query}'
        ' deadline = time.monotonic() + 2\n'
        ' reply = b""\n'
        f' while reply.count(b"R") < {repeats}:\n'
        '  if not select.select([0], [], [], max(0, deadline - time.monotonic()))[0]: break\n'
        '  data = os.read(0, 1)\n'
        '  if not data: break\n'
        '  reply += data\n'
        'finally:\n'
        ' termios.tcsetattr(0, termios.TCSANOW, old)\n'
        'print("CHILD_REPLY=" + repr(reply))\n'
    )
    return f'if [[ -n $LIG_PID ]]; then python3 -c {shlex.quote(program)}; fi'


class OrderedCursorFish(Fish):
    """Answer from observed wire bytes, independently of how long zsh takes to start."""

    DSR = b'\x1b[6n'

    def __init__(self, *args, drop_startup_reply=False, resize=False, split=False, **kwargs):
        self.query_stream = b''
        self.cursor_queries = 0
        self.device_queries = 0
        self.drop_startup_reply = drop_startup_reply
        self.resize = resize
        self.split = split
        self.resized = False
        super().__init__(*args, **kwargs)

    def answer(self, part):
        self.query_stream += part
        _, first, after = self.query_stream.partition(self.DSR)
        if first and self.resize and not self.resized and (
            after.endswith(self.DSR[:-1]) if self.split else b'CHILD_DSR_READY' in after
        ):
            self.resized = True
            fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 100, 1000, 600))
            os.kill(self.pid, signal.SIGWINCH)
        device_queries = self.query_stream.count(b'\x1b[0c')
        for _ in range(device_queries - self.device_queries):
            os.write(self.fd, b'\x1b[?1;2c')
        self.device_queries = device_queries

        cursor_queries = self.query_stream.count(self.DSR)
        while self.cursor_queries < cursor_queries:
            self.cursor_queries += 1
            if self.cursor_queries == 1:
                if not self.drop_startup_reply:
                    os.write(self.fd, b'\x1b[7;8R')
            else:
                os.write(self.fd, b'\x1b[9;10R')


class PromptOriginFish(Fish):
    """Report a nonzero terminal origin and retain the output for replay in tmux."""

    def answer(self, part):
        self.wire = getattr(self, 'wire', b'') + part
        if not getattr(self, 'answered_origin', False) and b'\x1b[6n' in self.wire:
            self.answered_origin = True
            os.write(self.fd, b'\x1b[7;8R')


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

    def test_midnight_commanders_subshell_is_not_wrapped_again(self):
        """Issue #66: mc's subshell has a terminal of its own and MC_SID set; a second lig there
        made mc start slowly and broke its Ctrl+O shell. A shell on a new terminal without it,
        like a tmux pane, still gets its own lig."""
        spawn = shlex.quote('import pty, sys; pty.spawn(sys.argv[1:])')
        shells = {'bash': shutil.which('bash'), 'zsh': os.environ.get('LIG_TEST_ZSH') or shutil.which('zsh'),
                  'fish': shutil.which('fish')}
        for name, shell in shells.items():
            if not shell:
                continue
            with self.subTest(name):
                term = Fish(wrapped=True, shell=shell)
                starts = lambda: sum(p.read_text(errors='replace').count('startup shell=')
                                     for p in term.home.rglob('lig.log'))
                # Printed, not typed: `inner-""ready` echoes as `inner-ready`.
                seen = lambda marker: term.plain().count(marker.encode())
                try:
                    self.assertEqual(starts(), 1)
                    for env, expected in [('MC_SID=4242 ', 1), ('', 2)]:
                        done = seen('inner-done')
                        term.send(f'{env}{sys.executable} -c {spawn} {shell} -i; echo inner-""done\r')
                        # The inner shell reads keys only once it has started (fish 4 asks the
                        # terminal first): until then, what's typed can be lost.
                        ready = seen('inner-ready')
                        for _ in range(10):
                            term.send('echo inner-""ready\r')
                            try:
                                term.wait_for(lambda: seen('inner-ready') > ready, timeout=2)
                                break
                            except AssertionError:
                                pass
                        self.assertGreater(seen('inner-ready'), ready, 'the inner shell never ran a command')
                        if expected == 2:
                            term.wait_for(lambda: starts() == 2)
                        self.assertEqual(starts(), expected, f'{env or "no MC_SID"}: lig sessions')
                        term.send('exit\r')
                        term.wait_for(lambda: seen('inner-done') > done)
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

    def test_startup_cursor_reply_precedes_a_child_query(self):
        """Lig's CPR must not be delivered to a startup program that asks after it."""
        shell = os.environ.get('LIG_TEST_ZSH') or shutil.which('zsh')
        if not shell:
            self.skipTest('zsh is unavailable')
        term = OrderedCursorFish(wrapped=True, shell=shell, extra_config=zsh_cursor_query(repeats=2))
        try:
            term.wait_text("CHILD_REPLY=b'\\x1b[9;10R\\x1b[9;10R'")
            self.assertEqual(term.buffer(), '')
        finally:
            self.assertEqual(term.close(17), 17)

    def test_child_cursor_query_waits_when_startup_reply_is_absent(self):
        shell = os.environ.get('LIG_TEST_ZSH') or shutil.which('zsh')
        if not shell:
            self.skipTest('zsh is unavailable')
        for resize, split in ((False, False), (True, False), (True, True)):
            with self.subTest(resize=resize, split=split):
                term = OrderedCursorFish(
                    wrapped=True, shell=shell, extra_config=zsh_cursor_query(split=split),
                    drop_startup_reply=True, resize=resize, split=split,
                )
                try:
                    term.wait_text("CHILD_REPLY=")
                    self.assertIn(b"CHILD_REPLY=b'\\x1b[9;10R'", term.plain())
                    self.assertEqual(term.resized, resize)
                    if split:
                        self.assertNotIn(term.DSR[:-1] + b'\x1b', term.query_stream)
                    self.assertEqual(term.buffer(), '')
                finally:
                    self.assertEqual(term.close(17), 17)

    def test_startup_origin_matches_tmux_after_an_osc133_prompt_mark(self):
        shell = os.environ.get('LIG_TEST_ZSH') or shutil.which('zsh')
        if not shell or not shutil.which('tmux'):
            self.skipTest('zsh and tmux are required')
        config = "unsetopt prompt_sp prompt_cr\nprecmd() { print -rn -- $'\\e]133;A\\a'; }\n"
        term = PromptOriginFish(wrapped=True, shell=shell, extra_config=config)
        base = ['tmux', '-S', str(term.home / 'socket'), '-f', '/dev/null']

        def tmux(*args):
            return subprocess.check_output(base + list(args), text=True, timeout=5)

        try:
            self.assertTrue(term.answered_origin)
            term.send('git che')
            term.wait_popup('checkout')
            wire = term.home / 'wire'
            wire.write_bytes(term.wire)
            # Replay the exact bytes into a terminal initialized at the
            # position reported by the startup CPR.
            play = (
                "import os,sys,time,tty; tty.setraw(0); os.write(1,b'\\x1b[7;8H'); "
                "os.write(1,open(sys.argv[1],'rb').read()); time.sleep(10)"
            )
            tmux('new-session', '-d', '-x', '80', '-y', '24',
                 shlex.join([sys.executable, '-c', play, str(wire)]))
            deadline = time.monotonic() + 5
            while True:
                lines = tmux('capture-pane', '-p').splitlines()
                if any('checkout' in line for line in lines):
                    break
                self.assertLess(time.monotonic(), deadline, lines)
                time.sleep(.02)
            prompt = 'test> git che'
            row = next(i for i, line in enumerate(lines) if prompt in line)
            col = lines[row].index(prompt) + len(prompt)
            menu = next(i for i, line in enumerate(lines) if 'checkout' in line)
            self.assertEqual(menu, row + 2, lines)
            self.assertEqual(tmux('display-message', '-p', '#{cursor_y},#{cursor_x}').strip(), f'{row},{col}')
        finally:
            subprocess.run(base + ['kill-server'], capture_output=True, timeout=5)
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
