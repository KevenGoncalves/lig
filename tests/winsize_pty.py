#!/usr/bin/env python3
"""Programs in lig see the terminal's size in pixels too, as kitty's image tools need (issue
#53), at the start and after a resize. Run after cargo build --release: python3 tests/winsize_pty.py."""
import fcntl
import os
import pty
import select
import shutil
import signal
import struct
import tempfile
import termios
import time
import unittest
from pathlib import Path
from fish_pty import BIN

PROBE = (b"python3 -c 'import fcntl,termios,struct; "
         b"print(\"winsize\", *struct.unpack(\"HHHH\", fcntl.ioctl(0, termios.TIOCGWINSZ, bytes(8))))'\r")


@unittest.skipUnless(shutil.which('bash') and shutil.which('python3'), 'bash and python3 are required')
class WinsizePty(unittest.TestCase):
    def setUp(self):
        directory = Path(tempfile.mkdtemp(prefix='lig-winsize-'))
        self.addCleanup(shutil.rmtree, directory, True)
        runtime = directory / 'runtime'
        runtime.mkdir(mode=0o700)
        (directory / '.bashrc').write_text("PS1='ws> '\n")
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.execve(BIN, [BIN, 'shell', '--shell', shutil.which('bash')],
                      {'HOME': str(directory), 'PATH': os.environ['PATH'], 'TERM': 'xterm-256color',
                       'XDG_RUNTIME_DIR': str(runtime), 'XDG_STATE_HOME': str(directory / 'state')})
        self.addCleanup(lambda: (os.kill(self.pid, signal.SIGKILL), os.waitpid(self.pid, 0)))
        self.resize(40, 120, 1200, 800)
        self.output = b''
        self.read_until(b'ws> ')

    def resize(self, rows, cols, width, height):
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, width, height))
        os.kill(self.pid, signal.SIGWINCH)

    def read_until(self, needle, timeout=10):
        deadline = time.monotonic() + timeout
        while needle not in self.output and time.monotonic() < deadline:
            if select.select([self.fd], [], [], .05)[0]:
                try:
                    data = os.read(self.fd, 65536)
                except OSError:
                    break
                self.output += data
                # Answer cursor position queries, as a terminal would.
                if b'\x1b[6n' in data:
                    os.write(self.fd, b'\x1b[1;1R')
        return needle in self.output

    def winsize(self):
        self.output = b''
        os.write(self.fd, PROBE)
        self.assertTrue(self.read_until(b'winsize '), self.output)
        self.read_until(b'\n', 2)
        line = self.output[self.output.index(b'winsize '):].split(b'\r')[0]
        return tuple(int(n) for n in line.split()[1:5])

    def test_pixels_reach_the_shell(self):
        self.assertEqual(self.winsize(), (40, 120, 1200, 800))
        self.resize(30, 100, 1000, 600)
        time.sleep(1)
        self.assertEqual(self.winsize(), (30, 100, 1000, 600))


if __name__ == '__main__':
    unittest.main(verbosity=2)
