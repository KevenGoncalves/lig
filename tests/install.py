#!/usr/bin/env python3
"""Offline installer coverage with synthetic releases and native checksum tools."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]

class Installer(unittest.TestCase):
    def install(self, system, arch, target, corrupt=False):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            bin = root / 'bin'
            bin.mkdir()
            for tool in ['sh', 'tar', 'mkdir', 'cp', 'chmod', 'mv', 'rm', 'mktemp', 'shasum', 'gzip']:
                found = shutil.which(tool)
                self.assertIsNotNone(found, tool)
                (bin / tool).symlink_to(found)
            name = f'lig-1.1.0-{target}'
            package = root / name
            package.mkdir()
            (package / 'lig').write_text('#!/bin/sh\necho lig 1.1.0\n')
            (package / 'lig').chmod(0o755)
            archive = root / 'release.tar.gz'
            with tarfile.open(archive, 'w:gz') as tar:
                tar.add(package, arcname=name)
            checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
            (root / 'release.sha256').write_text(f'{"0" * 64 if corrupt else checksum}  {name}.tar.gz\n')
            (bin / 'uname').write_text(f'#!/bin/sh\ncase "$1" in -s) echo {system};; -m) echo {arch};; esac\n')
            (bin / 'curl').write_text('#!/bin/sh\n# Installer invokes curl -fsSL -o DEST URL\nprintf "%s\\n" "$4" >> "$FIXTURE/urls"\ncase "$4" in *.sha256) cp "$FIXTURE/release.sha256" "$3";; *) cp "$FIXTURE/release.tar.gz" "$3";; esac\n')
            for tool in ['uname', 'curl']:
                (bin / tool).chmod(0o755)
            env = dict(os.environ, PATH=str(bin), FIXTURE=str(root), HOME=str(root),
                       LIG_VERSION='1.1.0', LIG_INSTALL_DIR=str(root / 'installed'))
            result = subprocess.run(['/bin/sh', str(ROOT / 'install.sh')], env=env, capture_output=True, text=True)
            return result, (root / 'installed/lig').exists(), (root / 'urls').read_text() if (root / 'urls').exists() else ''

    def test_apple_silicon_uses_darwin_and_shasum(self):
        result, installed, urls = self.install('Darwin', 'arm64', 'aarch64-apple-darwin')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(installed)
        self.assertIn('aarch64-apple-darwin.tar.gz', urls)
        self.assertIn('.sha256', urls)

    def test_linux_targets_remain_static(self):
        for arch, target in [('x86_64', 'x86_64-unknown-linux-musl'), ('aarch64', 'aarch64-unknown-linux-musl')]:
            result, installed, urls = self.install('Linux', arch, target)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(installed)
            self.assertIn(target, urls)

    def test_intel_mac_is_rejected_before_download(self):
        result, installed, urls = self.install('Darwin', 'x86_64', 'unused')
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(installed)
        self.assertEqual(urls, '')

    def test_bad_checksum_does_not_install(self):
        result, installed, _ = self.install('Darwin', 'arm64', 'aarch64-apple-darwin', corrupt=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('checksum', result.stderr)
        self.assertFalse(installed)

if __name__ == '__main__':
    unittest.main(verbosity=2)
