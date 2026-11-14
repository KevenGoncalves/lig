#!/usr/bin/env python3
"""`lig specs new` for a shell script, which the popup never runs `--help` for (issue #35)."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
LIG = Path(os.environ.get('LIG_TEST_BIN', ROOT / 'target/release/lig'))

# Like mole: the top-level help writes its name (`mo`) before each command, and a subcommand's
# subcommands answer `--help` with their parent's help.
SCRIPT = r'''#!/bin/sh
for a in "$@"; do [ "$a" = --help ] && help=1; done
[ -z "$help" ] && { echo "ran $*" >> "$(dirname "$0")/ran"; exit 0; }
case "$1" in
--help) printf '%s\n' 'COMMANDS' '  mo                 Main menu' '  mo clean           Free up disk space' \
    '  mo touchid         Configure Touch ID' '  mo --version       Show version' '' 'OPTIONS' '  --debug            Show logs' ;;
clean) printf '%s\n' 'Usage: mo clean [OPTIONS]' '' 'Options:' '  -n, --dry-run     Preview cleanup' ;;
touchid) printf '%s\n' 'Usage: mo touchid [COMMAND]' '' 'Commands:' '  enable            Enable Touch ID' \
    '  disable           Disable Touch ID' '' 'Options:' '  --dry-run         Preview changes' ;;
esac
'''


class SpecsNew(unittest.TestCase):
    def test_shell_script_spec_reads_every_level_of_help(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            bin = root / 'bin'
            bin.mkdir()
            (bin / 'mole').write_text(SCRIPT)
            (bin / 'mole').chmod(0o755)
            env = dict(os.environ, HOME=str(root), XDG_CONFIG_HOME=str(root / 'config'),
                       XDG_CACHE_HOME=str(root / 'cache'), PATH=f'{bin}:{os.environ["PATH"]}')
            # The popup first: it won't run the script, and caches that it found nothing.
            subprocess.run([LIG, 'complete', 'mole '], env=env, capture_output=True, check=True)
            result = subprocess.run([LIG, 'specs', 'new', 'mole'], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('reading `mole touchid --help`', result.stdout)
            spec = json.loads((root / 'config/lig/specs/mole.json').read_text())
            self.assertEqual([o['name'] for o in spec['options']], ['--version', '--debug'])
            subs = {s['name']: s for s in spec['subcommands']}
            self.assertEqual(list(subs), ['clean', 'touchid'])
            self.assertEqual(subs['clean']['description'], 'Free up disk space')
            self.assertEqual([o['name'] for o in subs['clean']['options']], [['-n', '--dry-run']])
            self.assertEqual([s['name'] for s in subs['touchid']['subcommands']], ['enable', 'disable'])
            for sub in subs['touchid']['subcommands']:
                self.assertNotIn('subcommands', sub)
            self.assertNotIn('loadHelp', json.dumps(spec))
            self.assertFalse((bin / 'ran').exists(), 'a command ran without --help')


if __name__ == '__main__':
    unittest.main(verbosity=2)
