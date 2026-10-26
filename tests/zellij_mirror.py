#!/usr/bin/env python3
"""zellij mirror end-to-end tests: lig rewrites its own argv and cwd to mirror the
foreground app. Run after cargo build --release: python3 tests/zellij_mirror.py."""
import os
import shutil
import time
import unittest

from fish_pty import BIN, Fish

ZSH = os.environ.get('LIG_TEST_ZSH') or shutil.which('zsh')
PAD = 4096


def cmdline(pid):
    with open(f'/proc/{pid}/cmdline', 'rb') as f:
        return f.read()


def args(pid):
    # What zellij sees: sysinfo splits on NUL and drops empty segments.
    return [a.decode(errors='replace') for a in cmdline(pid).split(b'\0') if a]


def wait_cmdline(pid, pred, timeout=5):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        last = args(pid)
        if pred(last):
            return last
        time.sleep(.05)
    raise AssertionError(f'timed out waiting on /proc/{pid}/cmdline; last: {last!r}')


class ZellijMirror(unittest.TestCase):
    def setUp(self):
        if not ZSH:
            if os.environ.get('CI'):
                self.fail('zsh is unavailable (required in CI)')
            self.skipTest('zsh is unavailable')
        self.saved = {k: os.environ.get(k) for k in ['ZELLIJ', 'LIG_NO_MIRROR']}
        os.environ.pop('LIG_NO_MIRROR', None)
        os.environ['ZELLIJ'] = '0'
        self.term = None

    def tearDown(self):
        if self.term:
            self.term.close()
        for key, value in self.saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value

    def start(self):
        # Fish copies os.environ at construction; zsh's pre-hook execs lig, so pid is lig.
        self.term = Fish(wrapped=True, shell=ZSH)
        return self.term.pid

    def test_idle_is_shell(self):
        pid = self.start()
        wait_cmdline(pid, lambda a: a == [ZSH])

    def test_app_and_cwd(self):
        pid = self.start()
        wait_cmdline(pid, lambda a: a == [ZSH])
        self.term.send('cd /tmp && sleep 30\r')
        wait_cmdline(pid, lambda a: a == ['sleep', '30'])
        self.assertEqual(os.readlink(f'/proc/{pid}/cwd'), '/tmp')
        self.term.send(b'\x03')
        wait_cmdline(pid, lambda a: a == [ZSH])

    def test_single_pad(self):
        pid = self.start()
        # No pad is under 4096 bytes, a double pad at least 8192.
        n = len(cmdline(pid))
        self.assertTrue(PAD <= n < 2 * PAD, n)

    def test_comm_is_lig(self):
        # The re-exec goes through /proc/self/exe, which would name the process "exe";
        # ps, htop, pgrep -x lig and killall lig rely on it staying "lig".
        pid = self.start()
        wait_cmdline(pid, lambda a: a == [ZSH])
        with open(f'/proc/{pid}/comm') as f:
            self.assertEqual(f.read().strip(), 'lig')

    def test_no_zellij_untouched(self):
        os.environ.pop('ZELLIJ', None)
        pid = self.start()
        # Longer than a mirror tick (1 s), so a running mirror would have rewritten it.
        self.term.read(1.5)
        a = args(pid)
        self.assertEqual(a[0], str(BIN))
        self.assertIn('shell', a)
        self.assertLess(len(cmdline(pid)), PAD)

    def test_no_mirror_untouched(self):
        # The kill switch: in zellij, but LIG_NO_MIRROR=1 means no pad and no mirror.
        os.environ['LIG_NO_MIRROR'] = '1'
        pid = self.start()
        self.term.read(1.5)
        a = args(pid)
        self.assertEqual(a[0], str(BIN))
        self.assertIn('shell', a)
        self.assertLess(len(cmdline(pid)), PAD)


if __name__ == '__main__':
    unittest.main(verbosity=2)
