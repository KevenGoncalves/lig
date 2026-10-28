#!/usr/bin/env python3
"""Linux PTY regression tests. Run after cargo build --release: python3 tests/fish_pty.py."""
import os
import pty
import re
import select
import signal
import shutil
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get('LIG_TEST_BIN', ROOT / 'target/release/lig')).resolve()
FISH = shutil.which('fish')

def fish_quote(value):
    return "'" + str(value).replace('\\', '\\\\').replace("'", "\\'") + "'"

class Fish:
    def __init__(self, wrapped=False, vi=False, shell=None, extra_config="", disabled=False, complete_commands=False, after_config=""):
        shell = shell or FISH
        if not shell:
            raise unittest.SkipTest('fish is unavailable')
        kind = Path(shell).name
        self.tmp = tempfile.TemporaryDirectory(prefix='lig-fish-test-')
        self.home = Path(self.tmp.name)
        self.env = dict(os.environ, HOME=str(self.home), XDG_CONFIG_HOME=str(self.home / 'config'),
                        XDG_DATA_HOME=str(self.home / 'data'), XDG_STATE_HOME=str(self.home / 'state'),
                        XDG_RUNTIME_DIR=str(self.home / 'runtime'),
                        PATH=str(BIN.parent) + os.pathsep + os.environ.get('PATH', ''),
                        SHELL=shell, TERM='xterm-256color', LIG_INSERT_FILE=str(self.home / 'insert'),
                        LIG_QUERY_FILE=str(self.home / 'query'))
        for key in ['LIG_DISABLE', 'LIG_TTY', 'LIG_PID', 'INSIDE_EMACS']:
            self.env.pop(key, None)
        (self.home / 'runtime').mkdir(mode=0o700)
        if disabled:
            self.env['LIG_DISABLE'] = '1'
        self.config = self.home / 'config/fish/config.fish'
        if kind != 'fish':
            self.config = self.home / ('.bashrc' if kind == 'bash' else '.zshrc')
            self.env['ZDOTDIR'] = str(self.home)
        self.config.parent.mkdir(parents=True, exist_ok=True)
        self.config.write_text("set -g fish_greeting\nfunction fish_prompt; printf 'test> '; end\nbind ctrl-o 'commandline --current-buffer > \"$HOME/buffer\"'\n" +
                               ('fish_vi_key_bindings\n' if vi else ''))
        if kind == 'bash':
            self.config.write_text("PS1='test> '\nbind -x '\"\\C-o\": printf \"%s\\n\" \"$READLINE_LINE\" > \"$HOME/buffer\"'\n")
        elif kind == 'zsh':
            self.config.write_text(extra_config + "\nPROMPT='test> '\n__test_buffer() { print -r -- \"$BUFFER\" > \"$HOME/buffer\"; }\nzle -N __test_buffer\nbindkey '^O' __test_buffer\n")

        if complete_commands:
            config = self.home / 'config/lig/config.toml'
            config.parent.mkdir(parents=True)
            config.write_text('[behavior]\ncomplete_commands = true\n')
        if wrapped:
            subprocess.run([BIN, 'install', kind], env=self.env, check=True, capture_output=True)
        if after_config:
            with self.config.open('a') as config:
                config.write(after_config)
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            if wrapped:
                os.execve(shell, [kind, '-i'], self.env)
            self.env['LIG_TTY'] = os.ttyname(0)
            os.execve(shell, ['fish', '-i', '-C', f'source {fish_quote(ROOT / "shell/post.fish")}'], self.env)
        self.wrapped = wrapped
        self.snapshot_id = 0
        self.output = b''
        self.wait_for(lambda: b'test> ' in self.output)

    def read(self, duration=.16):
        result = b''
        until = time.monotonic() + duration
        while time.monotonic() < until:
            if select.select([self.fd], [], [], .02)[0]:
                try:
                    part = os.read(self.fd, 65536)
                except OSError:
                    break
                if not part:
                    break
                result += part
                if b'\x1b[0c' in part:
                    os.write(self.fd, b'\x1b[?1;2c')
                if b'\x1b[6n' in part:
                    os.write(self.fd, b'\x1b[1;1R')
        result = result.replace(b'\r\n', b'\n')
        self.output = getattr(self, 'output', b'') + result
        return result

    def send(self, data):
        os.write(self.fd, data.encode() if isinstance(data, str) else data)
        return self.read()

    def wait_for(self, predicate, timeout=3):
        deadline = time.monotonic() + timeout
        while not predicate():
            if time.monotonic() >= deadline:
                raise AssertionError('timed out waiting for shell state')
            self.read(.02)

    def wait_text(self, text, start=0):
        if isinstance(text, str):
            text = text.encode()
        self.wait_for(lambda: text in re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]', b'', self.output[start:]))
        return self.output[start:]

    def snapshot(self):
        self.snapshot_id += 1
        (self.home / 'query').write_text(str(self.snapshot_id))
        start = len(self.output)
        os.kill(self.pid, signal.SIGUSR1)
        prefix = f'\x1b]6973;Q;{self.snapshot_id};'.encode()
        self.wait_for(lambda: prefix in self.output[start:] and b'\x07' in self.output[start:].split(prefix, 1)[1])
        return re.findall(re.escape(prefix) + rb'(.*?)\x07', self.output[start:], re.S)

    def buffer(self):
        path = self.home / 'buffer'
        path.unlink(missing_ok=True)
        os.write(self.fd, b'\x0f')
        self.wait_for(lambda: path.exists() and path.stat().st_size > 0)
        return path.read_text().strip()

    def insert(self, payload, expected=None):
        if expected is None:
            state = next(message for message in self.snapshot() if message.startswith(b'B;')).decode()
            _, cursor, buffer = state.split(';', 2)
            expected = buffer[:int(cursor)]
        delete, back, text = payload.split(';', 2)
        (self.home / 'insert').write_text(f'{delete};{back};{len(expected)};{expected}{text}')
        return self.send(b'\x1b[24;8~')

    def close(self, status=0):
        self.send(b'\x03')
        self.send(f'exit {status}\r')
        deadline = time.monotonic() + 3
        exit_status = None
        while time.monotonic() < deadline:
            self.read(.02)
            found, result = os.waitpid(self.pid, os.WNOHANG)
            if found:
                exit_status = os.waitstatus_to_exitcode(result)
                break
        if exit_status is None:
            os.kill(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
        os.close(self.fd)
        self.tmp.cleanup()
        if exit_status is None:
            raise AssertionError('shell did not exit')
        return exit_status

class FishTests(unittest.TestCase):
    def setUp(self):
        self.fish = Fish(vi=self._testMethodName == 'test_vi_modes')

    def tearDown(self):
        self.fish.close()

    def test_buffer_unicode_and_midline(self):
        self.fish.send('echo café🙂 tail')
        self.assertIn('B;15;echo café🙂 tail'.encode(), self.fish.snapshot())
        self.fish.send(b'\x1b[D' * 5)
        self.assertIn('B;10;echo café🙂 tail'.encode(), self.fish.snapshot())
        self.assertIn('B;10;echo café🙂 tail'.encode(), self.fish.snapshot())

    def test_multiline_and_trailing_newlines(self):
        self.fish.send(b'\x1b[200~echo a\n\n\x1b[201~')
        self.assertIn(b'B;8;echo a\n\n', self.fish.snapshot())
        self.fish.insert('0;0;X')
        self.assertIn(b'B;9;echo a\n\nX', self.fish.snapshot())

    def test_insert_suffix_back_and_no_execution(self):
        self.fish.send('echo café tail')
        self.fish.send(b'\x1b[D' * 5)
        self.fish.insert('4;1;hello')
        self.assertIn(b'B;9;echo hello tail', self.fish.snapshot())
        self.assertFalse((self.fish.home / 'executed').exists())
        self.fish.send(b'\x03')
        self.fish.send('touch ')
        self.fish.insert(f'0;0;{self.fish.home}/executed')
        self.assertFalse((self.fish.home / 'executed').exists())
        self.fish.send(b'\r')
        self.assertTrue((self.fish.home / 'executed').exists())

    def test_stale_insertion_leaves_the_buffer_unchanged(self):
        self.fish.send('git che')
        output = self.fish.insert('2;0;checkout', expected='git ch')
        self.assertIn(b'\x1b]6973;J\x07', output)
        self.assertIn(b'B;7;git che', self.fish.snapshot())

    def test_invalid_payload(self):
        self.fish.send('echo safe')
        self.fish.insert('bad;0;danger')
        self.assertIn(b'B;9;echo safe', self.fish.snapshot())

    def test_vi_modes(self):
        self.fish.send('echo test')
        self.assertIn(b'B;9;echo test', self.fish.snapshot())
        self.fish.send(b'\x1b')
        self.assertIn(b'H', self.fish.snapshot())
        self.fish.send('i')
        self.assertTrue(any(m.startswith(b'B;') for m in self.fish.snapshot()))
        self.fish.insert('0;0;X')
        self.assertTrue(any(b'X' in m for m in self.fish.snapshot()))

    def test_history_and_pager(self):
        self.fish.send('echo previous\r')
        self.fish.send(b'\x1b[A')
        self.assertIn(b'H', self.fish.snapshot())
        self.fish.send(b'\x03')
        self.fish.send('git --')
        self.fish.send(b'\t\t')
        self.assertIn(b'H', self.fish.snapshot())

    def test_autosuggestion_and_mode_switch(self):
        self.fish.send('echo autosuggestion_test\r')
        self.fish.send('echo autosuggestion_')
        self.fish.snapshot()
        self.fish.send(b'\x1b[C')
        self.assertIn(b'B;24;echo autosuggestion_test', self.fish.snapshot())
        self.fish.send(b'\x03fish_vi_key_bindings\r')
        self.fish.send('echo switch')
        self.fish.insert('0;0;X')
        self.assertIn(b'B;12;echo switchX', self.fish.snapshot())

    def test_multiline_payload_and_control_sanitizing(self):
        self.fish.send('echo tail')
        self.fish.send(b'\x1b[D' * 4)
        self.fish.insert('0;0;"café;🙂"\n')
        self.assertIn('B;14;echo "café;🙂"\ntail'.encode(), self.fish.snapshot())
        self.fish.send(b'\x03')
        self.fish.insert('0;0;echo A\x07B\x1bC')
        self.assertIn(b'B;10;echo A B C', self.fish.snapshot())

    def test_finish_and_cwd(self):
        out = self.fish.send('cd /tmp\r')
        self.assertIn(b'\x1b]6973;F\x07', out)
        self.assertIn(b'\x1b]6973;C;/tmp\x07', out)
        self.assertRegex(out, rb'\x1b\]6973;R;[0-9]+\x07')

class WrappedTests(unittest.TestCase):
    def test_missing_snapshot_reply_recovers_without_a_new_prompt(self):
        hook = r"""
functions --erase __lig_report
function __lig_report --on-signal USR1
    set -l request (string collect < "$LIG_QUERY_FILE")
    if not set -q __test_dropped
        set -g __test_dropped 1
        return
    end
    printf '\e]6973;Q;%s;B;%s;%s\a' "$request" (commandline --cursor) (commandline | string collect --allow-empty) >/dev/tty
    printf %s "$request" > "$HOME/request-id"
end
"""
        fish = Fish(wrapped=True, after_config=hook)
        try:
            fish.send('git ch')
            fish.wait_for(lambda: b'checkout' in re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]', b'', fish.output))
            self.assertGreater(int((fish.home / 'request-id').read_text()), 1)
        finally:
            fish.close()

    def test_fish_started_inside_another_shell(self):
        zsh = os.environ.get('LIG_TEST_ZSH') or shutil.which('zsh')
        if not zsh:
            self.skipTest('zsh is unavailable')
        term = Fish(wrapped=True, shell=zsh)
        try:
            config = term.home / 'config/fish/config.fish'
            config.parent.mkdir(parents=True, exist_ok=True)
            config.write_text("set -g fish_greeting\nfunction fish_prompt; printf 'nested> '; end\n"
                              "bind ctrl-o 'commandline --current-buffer > \"$HOME/buffer\"'\n")
            subprocess.run([BIN, 'install', 'fish'], env=term.env, check=True, capture_output=True)
            start = len(term.output)
            term.send('fish\r')
            term.wait_for(lambda: b'nested> ' in term.output[start:])
            start = len(term.output)
            term.send('git ch')
            term.wait_text('checkout', start)
            term.send(b'\t')
            term.send(b'\r')
            self.assertEqual(term.buffer(), 'git cherry-pick')
            term.send(b'\x03')
            term.send('exit\r')
        finally:
            term.close()

    def test_fast_enter_uses_the_updated_buffer(self):
        fish = Fish(wrapped=True)
        try:
            for _ in range(8):
                fish.send(b'\x03')
                start = len(fish.output)
                fish.send('git ch')
                fish.wait_text('checkout', start)
                # Enter arrives in a separate read before the snapshot reply.
                os.write(fish.fd, b'e')
                time.sleep(.002)
                os.write(fish.fd, b'\r')
                fish.read(.25)
                self.assertEqual(fish.buffer(), 'git checkout')
        finally:
            fish.close()

    def test_commands_subcommands_and_flags(self):
        fish = Fish(wrapped=True, complete_commands=True)
        try:
            for prefix, expected in [('car', 'cargo'), ('cargo bu', 'cargo build'),
                                     ('cargo build --rel', 'cargo build --release')]:
                fish.send(b'\x03')
                start = len(fish.output)
                fish.send(prefix)
                fish.wait_text(expected.split()[-1], start)
                fish.send(b'\r')
                self.assertEqual(fish.buffer(), expected)
        finally:
            fish.close()

    def test_reloading_config_continues_after_the_hook(self):
        fish = Fish(wrapped=True, after_config='set -q __test_count; or set -g __test_count 0\nset -g __test_count (math $__test_count + 1)\n')
        try:
            fish.send('source "$XDG_CONFIG_HOME/fish/config.fish"; printf %s $__test_count > "$HOME/reloads"\r')
            path = fish.home / 'reloads'
            fish.wait_for(path.exists)
            self.assertEqual(path.read_text(), '2')
        finally:
            fish.close()

    def test_disabled_session(self):
        fish = Fish(wrapped=True, disabled=True, after_config='printf reached > "$HOME/after-post"\n')
        try:
            fish.send('printf %s "$LIG_TTY" > "$HOME/tty"; functions -q __lig_report; echo $status > "$HOME/hook_status"\r')
            self.assertEqual((fish.home / 'after-post').read_text(), 'reached')
            self.assertEqual((fish.home / 'tty').read_text(), '')
            self.assertEqual((fish.home / 'hook_status').read_text().strip(), '1')
        finally:
            fish.close()

    def test_options_and_quoted_path_with_suffix(self):
        fish = Fish(wrapped=True)
        try:
            start = len(fish.output)
            fish.send('cargo build --rel')
            fish.wait_text('--release', start)
            fish.send(b'\r')
            self.assertEqual(fish.buffer(), 'cargo build --release')
            fish.send(b'\x03')
            target = fish.home / 'space file'
            target.touch()
            start = len(fish.output)
            fish.send(f'cat {fish.home}/spa tail')
            fish.send(b'\x1b[D' * 5)
            fish.wait_text('space file', start)
            fish.send(b'\r')
            buffer = fish.buffer()
            self.assertTrue(buffer.endswith(' tail'), buffer)
            # Let fish parse the arguments without running cat.
            parsed = subprocess.run([FISH, '--no-config', '-c', f'set -l args {buffer[4:]}; printf "%s\\n" $args'],
                                    env=fish.env, check=True, capture_output=True).stdout.decode().splitlines()
            self.assertEqual(parsed, [str(target), 'tail'])
        finally:
            fish.close()

    def test_popup_and_acceptance(self):
        fish = Fish(wrapped=True)
        try:
            start = len(fish.output)
            fish.send('git ch')
            out = fish.wait_text('checkout', start)
            plain = re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]', b'', out)
            self.assertIn(b'checkout', plain)
            fish.send(b'\x1b')  # dismiss
            fish.send(b'\x00')  # manual toggle
            # Tab selects; Enter accepts without running the command.
            fish.send(b'\t')
            fish.send(b'\r')
            self.assertEqual(fish.buffer(), 'git cherry-pick')
            out = fish.send(b'\x03echo WRAPPER_ALIVE\r')
            self.assertIn(b'WRAPPER_ALIVE', out)
            fish.send('lig doctor > "$HOME/doctor"\r')
            self.assertIn('[ok  ] this terminal is running inside lig', (fish.home / 'doctor').read_text())
        finally:
            fish.close()

    def test_typing_keeps_the_popup_on_screen(self):
        fish = Fish(wrapped=True)
        try:
            start = len(fish.output)
            fish.send('git ch')
            fish.wait_text('checkout', start)
            start = len(fish.output)
            fish.send('e')
            fish.read(.3)
            # Every frame lig finishes after the key still has the popup in it.
            frames = fish.output[start:].split(b'\x1b[?2026l')[:-1]
            self.assertTrue(frames)
            for frame in frames:
                self.assertIn('╭'.encode(), frame)
        finally:
            fish.close()

@unittest.skipUnless(FISH, 'fish is unavailable')
class CliTests(unittest.TestCase):
    def test_noninteractive_config_continues_after_the_hook(self):
        with tempfile.TemporaryDirectory(prefix='lig-config-test-') as directory:
            home = Path(directory)
            env = dict(os.environ, HOME=directory, XDG_CONFIG_HOME=str(home / 'config'), SHELL=FISH)
            env.pop('LIG_TTY', None)
            subprocess.run([BIN, 'install', 'fish'], env=env, check=True, capture_output=True)
            config = home / 'config/fish/config.fish'
            with config.open('a') as file:
                file.write('printf reached > "$HOME/after-post"\n')
            for disabled in ['', '1']:
                env['LIG_DISABLE'] = disabled
                (home / 'after-post').unlink(missing_ok=True)
                subprocess.run([FISH, '-c', 'true'], env=env, check=True)
                self.assertEqual((home / 'after-post').read_text(), 'reached')

    def test_install_uninstall_and_noninteractive(self):
        with tempfile.TemporaryDirectory(prefix='lig-config-test-') as directory:
            home = Path(directory)
            env = dict(os.environ, HOME=directory, XDG_CONFIG_HOME=str(home / 'custom'), SHELL=FISH)
            env.pop('LIG_TTY', None)
            config = home / 'custom/fish/config.fish'
            config.parent.mkdir(parents=True)
            original = "# personal config\nset -g greeting 'hello'\n\n"
            config.write_text(original)
            for attempt in range(2):
                subprocess.run([BIN, 'install'], env=env, check=True, capture_output=True)
                if attempt == 0:
                    installed = config.read_text()
                else:
                    self.assertEqual(config.read_text(), installed)
            self.assertEqual(config.with_name('config.fish.lig-backup').read_text(), original)
            report = subprocess.run([BIN, 'doctor'], env=env, check=True, capture_output=True).stdout
            self.assertIn(str(config).encode(), report)
            subprocess.run([FISH, '-c', 'test -z "$LIG_TTY"; and not functions -q __lig_report'], env=env, check=True)
            subprocess.run([BIN, 'uninstall', 'fish'], env=env, check=True, capture_output=True)
            self.assertEqual(config.read_text(), original)

if __name__ == '__main__':
    unittest.main(verbosity=2)
