#!/usr/bin/env python3
"""Opt-in real macOS resurrection test: LIG_TEST_ZELLIJ=/path/to/new/zellij python3 tests/zellij_resurrection.py."""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import shlex
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
import unittest
from fish_pty import BIN

ZELLIJ = os.environ.get('LIG_TEST_ZELLIJ')

@unittest.skipUnless(sys.platform == 'darwin' and ZELLIJ, 'requires macOS and LIG_TEST_ZELLIJ with set-resurrect-command')
class Resurrection(unittest.TestCase):
    def test_lig_restores_exact_command_and_cwd_after_enter(self):
        with tempfile.TemporaryDirectory(prefix='lig-zr-', dir='/tmp') as temp:
            root=Path(temp)
            zellij=str(Path(ZELLIJ).resolve())
            name='lig-test-'+str(os.getpid())+'-'+str(time.time_ns())
            env=dict(os.environ, HOME=str(root), ZDOTDIR=str(root), SHELL='/bin/zsh', TERM='xterm-256color',
                     TMPDIR=str(root), XDG_CONFIG_HOME=str(root/'config'),
                     ZELLIJ_SOCKET_DIR=str(root/'socket'),
                     PATH=str(Path(zellij).parent)+os.pathsep+str(BIN.parent)+os.pathsep+os.environ['PATH'])
            for key in ['ZELLIJ','ZELLIJ_SESSION_NAME','ZELLIJ_PANE_ID','LIG_TTY','LIG_PID','LIG_DISABLE','LIG_NO_MIRROR']:
                env.pop(key,None)
            config=root/'config.kdl'
            config.write_text('default_shell "/bin/zsh"\nshow_startup_tips false\nshow_release_notes false\nsession_serialization true\nserialization_interval 1\n')
            layout=root/'layout.kdl';layout.write_text('layout { pane; }\n')
            (root/'.zshrc').write_text("PROMPT='test> '\n")
            subprocess.run([str(BIN),'install','zsh'],env=env,check=True,capture_output=True)
            base=[zellij,'--config-dir',str(root),'--config',str(config)]
            clients=[]
            terminal_output=bytearray()
            def cli(*args):
                result=subprocess.run(base+['--session',name]+list(args),env=env,capture_output=True,text=True,timeout=10)
                self.assertEqual(result.returncode,0,result.stderr+repr(bytes(terminal_output[-3000:])))
                return result.stdout
            def wait(predicate, timeout=12):
                deadline=time.monotonic()+timeout
                while time.monotonic()<deadline:
                    value=predicate()
                    if value:return value
                    time.sleep(.1)
                self.fail('timed out waiting for zellij/Lig state')
            def attach(new=False):
                pid,fd=pty.fork()
                if pid==0:
                    args=base+(['--new-session-with-layout',str(layout),'--session',name] if new else ['attach',name])
                    os.execve(zellij,args,env)
                fcntl.ioctl(fd,termios.TIOCSWINSZ,struct.pack('HHHH',40,120,0,0))
                clients.append((pid,fd))
                def drain():
                    try:
                        while True:
                            data=os.read(fd,65536)
                            if not data:break
                            terminal_output.extend(data)
                            if b'\x1b[6n' in data:os.write(fd,b'\x1b[1;1R')
                    except OSError:pass
                threading.Thread(target=drain,daemon=True).start()
                time.sleep(2)
                wait(lambda:'terminal_' in cli('action','list-panes'))
                # Dismiss only the test home's onboarding plugin, never approve its setup actions.
                for plugin in re.findall(r'(plugin_\d+).*Welcome to Zellij',cli('action','list-panes')):
                    cli('action','close-pane','--pane-id',plugin)
                return fd
            def dump():return cli('action','dump-layout')
            try:
                fd=attach(True)
                # Lig should replace its own wrapper command with the shell at idle.
                wait(lambda:'command=' not in dump())
                cwd=root/'work dir';cwd.mkdir()
                output=root/'executed.json'
                script=root/'record args.py'
                script.write_text('import json,os,sys,time\njson.dump({"args":sys.argv[1:],"cwd":os.getcwd()},open('+repr(str(output))+',"w"))\ntime.sleep(60)\n')
                args=['two words','','a;echo BAD',"single'quote",'日本語','--flag']
                line='cd '+shlex.quote(str(cwd))+'; '+shlex.join([sys.executable,str(script),*args])+'\r'
                os.write(fd,line.encode())
                wait(output.exists)
                wait(lambda: 'two words' in dump() and 'record args.py' in dump())
                saved=dump()
                self.assertIn('start_suspended true',saved)
                self.assertIn(str(cwd),saved)
                cli('action','save-session')
                time.sleep(1)
                cli('kill-session',name)
                time.sleep(1)
                output.unlink()
                fd=attach(False)
                self.assertFalse(output.exists(),'command must remain suspended until Enter')
                os.write(fd,b'\r')
                wait(output.exists)
                result=json.loads(output.read_text())
                self.assertEqual(result['args'],args)
                self.assertEqual(Path(result['cwd']).resolve(),cwd.resolve())
            finally:
                subprocess.run(base+['kill-session',name],env=env,capture_output=True,timeout=10)
                for pid,fd in clients:
                    try:os.close(fd);os.waitpid(pid,0)
                    except OSError:pass

if __name__=='__main__':unittest.main(verbosity=2)
