"""Exercise ISO kernel selection without compiling or downloading a kernel."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

TOOLING = Path(__file__).resolve().parents[1]


class KernelSelectionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        tooling = self.root / 'tooling'
        tooling.mkdir()
        for name in ('env.sh', 'versions.env', 'verify-prebuilt-kernel.py'):
            shutil.copyfile(TOOLING / name, tooling / name)
        script = (TOOLING / 'build.sh').read_text()
        script = script[:script.rindex('\nif [[ "$stage" == all ]]')]
        self.script = tooling / 'build.sh'
        self.script.write_text(script + '\nselect_iso_kernel\n')
        self.build = self.root / '.build'
        (self.build / 'out').mkdir(parents=True)
        (self.build / 'downloads').mkdir()
        self.config = (TOOLING / 'kernel.config').read_bytes()
        image = bytearray(1024)
        image[0x1FE:0x200] = b'\x55\xaa'
        image[0x202:0x206] = b'HdrS'
        self.image = bytes(image)
        self.env = dict(os.environ, HOS_BUILD_DIR=str(self.build), HOS_KERNEL_MODE='source')

    def published(self, config=None, image=None):
        (self.build / 'out/vmlinuz').write_bytes(self.image if image is None else image)
        (self.build / 'out/kernel.config').write_bytes(self.config if config is None else config)

    def fallback(self, config=None):
        image = self.image + b'fallback'
        config = self.config if config is None else config
        for name, data, key in (
            ('vmlinuz', image, 'VMLINUZ'),
            ('kernel.config', config, 'KERNEL_CONFIG'),
        ):
            (self.build / f'downloads/prebuilt-{name}').write_bytes(data)
            self.env[f'HOS_PREBUILT_{key}_SHA256'] = hashlib.sha256(data).hexdigest()
            self.env[f'HOS_PREBUILT_{key}_URL'] = 'https://unused.invalid/' + name
        return image

    def run_selection(self):
        return subprocess.run(['bash', str(self.script), 'iso'], env=self.env,
                              text=True, capture_output=True)

    def test_existing_kernel_preferred_even_in_prebuilt_mode(self):
        self.published()
        self.fallback(b'CONFIG_64BIT=y\n')
        self.env['HOS_KERNEL_MODE'] = 'prebuilt'
        result = self.run_selection()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.build / 'out/vmlinuz').read_bytes(), self.image)

    def test_missing_kernel_uses_fallback_in_source_mode(self):
        expected = self.fallback()
        result = self.run_selection()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.build / 'out/vmlinuz').read_bytes(), expected)

    def test_missing_config_uses_fallback(self):
        (self.build / 'out/vmlinuz').write_bytes(self.image)
        expected = self.fallback()
        result = self.run_selection()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.build / 'out/vmlinuz').read_bytes(), expected)

    def test_incompatible_or_corrupt_kernel_uses_fallback(self):
        expected = self.fallback()
        for config, image in ((b'CONFIG_64BIT=y\n', self.image), (self.config, b'broken')):
            with self.subTest(image=image[:8]):
                self.published(config, image)
                result = self.run_selection()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((self.build / 'out/vmlinuz').read_bytes(), expected)

    def test_incompatible_fallback_is_rejected_without_replacing_output(self):
        self.published(b'CONFIG_64BIT=y\n')
        self.fallback(b'CONFIG_64BIT=y\n')
        result = self.run_selection()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Configured prebuilt kernel is incompatible', result.stderr)
        self.assertEqual((self.build / 'out/vmlinuz').read_bytes(), self.image)

    def test_fallback_checksum_mismatch_is_rejected(self):
        self.fallback()
        self.env['HOS_PREBUILT_VMLINUZ_SHA256'] = '0' * 64
        result = self.run_selection()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('checksum mismatch', result.stderr)
        self.assertFalse((self.build / 'out/vmlinuz').exists())


if __name__ == '__main__':
    unittest.main()
