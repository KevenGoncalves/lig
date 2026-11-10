#!/usr/bin/env python3
"""macOS mirror contract tests. Optional real resurrection: LIG_TEST_ZELLIJ=/path/to/zellij."""
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

from fish_pty import Fish, BIN

@unittest.skipUnless(sys.platform == 'darwin', 'macOS integration')
class MirrorContract(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='lig-zellij-contract-')
        self.root = Path(self.tmp.name)
        self.calls = self.root / 'calls'
        self.unsupported = self.root / 'unsupported'
        self.slow = self.root / 'slow'
        zellij = self.root / 'zellij'
        zellij.write_text(f'''#!{sys.executable}
import json,sys,time
from pathlib import Path
root=Path({str(self.root)!r})
if '--help' in sys.argv:
    sys.exit(1 if (root/'unsupported').exists() else 0)
if (root/'slow').exists():time.sleep(10)
with (root/'calls').open('a') as f:f.write(json.dumps(sys.argv[1:])+'\\n')
''')
        zellij.chmod(0o755)
        self.env = patch.dict(os.environ, ZELLIJ='0', ZELLIJ_SESSION_NAME='test session', ZELLIJ_PANE_ID='7',
                              PATH=str(self.root)+os.pathsep+os.environ['PATH'])
        self.env.start()
        self.term = None

    def tearDown(self):
        try:
            if self.term:self.term.close()
        finally:
            self.env.stop()
            self.tmp.cleanup()

    def start(self):
        self.term = Fish(wrapped=True, shell='/bin/zsh')
        return self.term

    def wait_call(self, predicate):
        deadline=time.monotonic()+6
        while time.monotonic()<deadline:
            calls=[json.loads(l) for l in self.calls.read_text().splitlines()] if self.calls.exists() else []
            for call in reversed(calls):
                if predicate(call):return call
            self.term.read(.1)
        self.fail(f'missing zellij update; calls={calls!r}')

    def test_foreground_arguments_cwd_and_idle(self):
        term=self.start()
        self.wait_call(lambda c:c[-1]=='/bin/zsh')
        cwd=term.home/'work dir';cwd.mkdir()
        script=term.home/'app.py'
        script.write_text('import time\ntime.sleep(30)\n')
        term.send(f"cd '{cwd}'; '{sys.executable}' '{script}' 'two words' '' 'a;echo bad'\r")
        call=self.wait_call(lambda c:'two words' in c)
        self.assertTrue(Path(call[call.index('--')+1]).is_file())
        self.assertEqual(call[call.index('--')+2:],[str(script),'two words','','a;echo bad'])
        self.assertEqual(Path(call[call.index('--cwd')+1]).resolve(),cwd.resolve())
        self.assertEqual(call[:5],['--session','test session','action','set-resurrect-command','--pane-id'])
        term.send(b'\x03')
        self.wait_call(lambda c:c[-1]=='/bin/zsh' and Path(c[c.index('--cwd')+1]).resolve()==cwd.resolve())

    def test_old_zellij_keeps_shell_and_reports_limitation(self):
        self.unsupported.touch()
        term=self.start()
        term.read(1.2)  # allow the asynchronous capability probe to finish
        term.send('lig doctor > "$HOME/doctor"\r')
        term.wait_for(lambda:(term.home/'doctor').exists() and 'lacks set-resurrect-command' in (term.home/'doctor').read_text(),timeout=6)
        self.assertFalse(self.calls.exists())
        term.send('printf works > "$HOME/alive"\r')
        term.wait_for(lambda:(term.home/'alive').exists())
        self.assertEqual((term.home/'alive').read_text(),'works')

    def test_explicit_disable_skips_zellij(self):
        with patch.dict(os.environ, LIG_NO_MIRROR='1'):
            term=self.start()
        term.read(1.2)
        self.assertFalse(self.calls.exists())
        term.send('lig doctor > "$HOME/doctor"\r')
        term.wait_for(lambda:(term.home/'doctor').exists() and 'LIG_NO_MIRROR set' in (term.home/'doctor').read_text())

    def test_stalled_updates_do_not_block_shell(self):
        self.slow.touch()
        term=self.start()
        term.send('printf responsive > "$HOME/alive"\r')
        term.wait_for(lambda:(term.home/'alive').exists())
        time.sleep(2.5)
        term.send('lig doctor > "$HOME/doctor"\r')
        term.wait_for(lambda:(term.home/'doctor').exists() and 'update failed' in (term.home/'doctor').read_text(),timeout=5)
        self.assertEqual((term.home/'alive').read_text(),'responsive')
        self.slow.unlink()
        self.wait_call(lambda c:c[-1]=='/bin/zsh')

if __name__=='__main__':
    unittest.main(verbosity=2)
