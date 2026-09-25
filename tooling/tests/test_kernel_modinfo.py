"""Check binary cleanup and permission failures on Windows-mounted builds."""
import errno
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    'modinfo', Path(__file__).resolve().parents[1] / 'fix-kernel-modinfo.py')
modinfo = importlib.util.module_from_spec(spec)
spec.loader.exec_module(modinfo)


class ModinfoTests(unittest.TestCase):
    def test_cleanup_matches_kbuild_sed(self):
        for data in (b'', b'foo.name=foo\0bar.name=bar\0\0\0',
                     b'foo.description=line\0\0\nnext\0\0', b'no padding'):
            with self.subTest(data=data), tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / 'modules.builtin.modinfo'
                path.write_bytes(data)
                path.chmod(0o755)
                expected = subprocess.check_output(
                    ['sed', r's/\x00\+$/\x00/g'], input=data)
                modinfo.fix_modinfo(path)
                self.assertEqual(path.read_bytes(), expected)
                self.assertEqual(path.stat().st_mode & 0o777, 0o644)

    def test_permission_errors(self):
        for code in (errno.EPERM, errno.EOPNOTSUPP, errno.ENOSYS, errno.EIO):
            with self.subTest(errno=code), tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / 'modules.builtin.modinfo'
                path.write_bytes(b'foo.name=foo\0\0\0')
                path.chmod(0o755)
                with patch.object(modinfo.os, 'fchmod', side_effect=OSError(code, 'test')):
                    if code == errno.EIO:
                        with self.assertRaises(OSError):
                            modinfo.fix_modinfo(path)
                    else:
                        modinfo.fix_modinfo(path)
                self.assertEqual(path.read_bytes(), b'foo.name=foo\0')


if __name__ == '__main__':
    unittest.main()
