import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile
import stat

spec=importlib.util.spec_from_file_location('support',Path(__file__).with_name('node_support.py'))
support=importlib.util.module_from_spec(spec);spec.loader.exec_module(support)

class NodeSupport(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name)
    def tearDown(self):self.temp.cleanup()
    def tar(self,entries):
        path=self.root/'fixture.tar.gz'
        with tarfile.open(path,'w:gz') as out:
            for name,data,kind in entries:
                item=tarfile.TarInfo(name);item.size=len(data);item.type=kind
                if kind==tarfile.SYMTYPE:item.linkname='/tmp/outside'
                out.addfile(item,io.BytesIO(data) if kind==tarfile.REGTYPE else None)
        return path
    def test_exact_members_only_and_no_npm_or_traversal_extraction(self):
        archive=self.tar([('node-fixture/bin/node',b'executable',tarfile.REGTYPE),('node-fixture/LICENSE',b'license',tarfile.REGTYPE),('../../outside',b'unsafe',tarfile.REGTYPE),('node-fixture/bin/npm',b'not included',tarfile.REGTYPE)])
        self.assertEqual(support.unpack(archive,'node-fixture.tar.gz',False),{'node':b'executable','LICENSE':b'license'})
        self.assertFalse((self.root/'outside').exists())
    def test_duplicate_or_linked_selected_member_refused(self):
        base=[('node-fixture/bin/node',b'executable',tarfile.REGTYPE),('node-fixture/LICENSE',b'license',tarfile.REGTYPE)]
        with self.assertRaises(ValueError):support.unpack(self.tar(base+[base[0]]),'node-fixture.tar.gz',False)
        with self.assertRaises(ValueError):support.unpack(self.tar([('node-fixture/bin/node',b'',tarfile.SYMTYPE),base[1]]),'node-fixture.tar.gz',False)
    def test_missing_license_refused(self):
        with self.assertRaises(ValueError):support.unpack(self.tar([('node-fixture/bin/node',b'node',tarfile.REGTYPE)]),'node-fixture.tar.gz',False)
    def test_windows_executable_is_regular_and_fixed_name(self):
        archive=self.root/'fixture.zip'
        with zipfile.ZipFile(archive,'w') as out:
            out.writestr('node-fixture/node.exe',b'PE fixture');out.writestr('node-fixture/LICENSE',b'license');out.writestr('node-fixture/npm.cmd',b'not copied')
        self.assertEqual(set(support.unpack(archive,'node-fixture.zip',True)),{'node.exe','LICENSE'})
        with zipfile.ZipFile(archive,'w') as out:
            item=zipfile.ZipInfo('node-fixture/node.exe');item.external_attr=(stat.S_IFLNK|0o777)<<16;out.writestr(item,'outside');out.writestr('node-fixture/LICENSE','license')
        with self.assertRaises(ValueError):support.unpack(archive,'node-fixture.zip',True)
    def test_prepare_preserves_input_and_refuses_used_destination(self):
        archive=self.tar([('node-fixture/bin/node',b'node',tarfile.REGTYPE),('node-fixture/LICENSE',b'license',tarfile.REGTYPE)])
        original=support.digest(archive);out=self.root/'output';row={'file':'node-fixture.tar.gz','sha256':original}
        with patch.object(support,'download',return_value=archive),patch.object(support,'asset_for',return_value=({'version':'22.23.5','source':'official'},row)):
            metadata=support.prepare('linux-x64',self.root/'cache',out)
            self.assertEqual(metadata['executable_sha256'],hashlib.sha256(b'node').hexdigest());self.assertEqual((out/'LICENSE').read_bytes(),b'license')
            with self.assertRaises(ValueError):support.prepare('linux-x64',self.root/'cache',out)
        self.assertEqual(support.digest(archive),original)
    def test_cached_checksum_mismatch_is_not_replaced_or_executed(self):
        lock,row=support.asset_for('linux-x64');archive=self.root/row['file'];archive.write_bytes(b'wrong')
        with self.assertRaisesRegex(ValueError,'checksum'):support.download('linux-x64',self.root)
        self.assertEqual(archive.read_bytes(),b'wrong')
    def test_selected_member_limit_applies_before_read(self):
        archive=self.tar([('node-fixture/bin/node',b'executable',tarfile.REGTYPE),('node-fixture/LICENSE',b'license',tarfile.REGTYPE)])
        with patch.object(support,'MAX_EXECUTABLE',2):
            with self.assertRaises(ValueError):support.unpack(archive,'node-fixture.tar.gz',False)
    def test_lock_has_exact_six_official_platforms(self):
        lock=json.loads((support.ROOT/'lock.json').read_text())
        self.assertEqual(set(lock['targets']),{'linux-x64','linux-arm64','macos-x64','macos-arm64','windows-x64','windows-arm64'})
        self.assertEqual(lock['version'],'22.23.5')
        self.assertEqual(lock['source'],'https://nodejs.org/dist/v22.23.5/SHASUMS256.txt')
        for row in lock['targets'].values():self.assertRegex(row['sha256'],r'^[a-f0-9]{64}$')

if __name__=='__main__':unittest.main()
