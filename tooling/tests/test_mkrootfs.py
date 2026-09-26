import contextlib
import gzip
import io
import pathlib
import runpy
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "mkrootfs.py"


def read_archive(path):
    archive = gzip.decompress(path.read_bytes())
    result = {}
    offset = 0
    while True:
        assert archive[offset:offset + 6] == b"070701"
        fields = [int(archive[offset + 6 + n * 8:offset + 14 + n * 8], 16) for n in range(13)]
        offset += 110
        name = archive[offset:offset + fields[11] - 1].decode()
        offset = (offset + fields[11] + 3) & ~3
        data = archive[offset:offset + fields[6]]
        offset = (offset + fields[6] + 3) & ~3
        if name == "TRAILER!!!":
            return result
        result[name] = (fields, data)


def make_fixture(root):
    """Write everything mkrootfs.py packages; return its arguments and ldd output."""
    (root / "out").mkdir()
    for name in ("coreutils", "coreutils-applets", "coreutils-LICENSE", "hos-password", "hos-init", "hoswm", "hos-hello", "hos-terminal", "hos-installer", "hos-about", "hos-files", "hos-image", "hos-notifications", "hos-toast", "hos-account", "hos-settings", "hos-notepad", "hos-snake", "hos-netd", "hos-power", "hos-ntpd", "hos-soundd", "hosctl", "hoswm.h", "libhoswm.a", "vmlinuz"):
        (root / "out" / name).write_bytes(name.encode())
    for name in ("hos-paint", "hos-colorpicker"):
        (root / "out" / name).write_bytes(name.encode())
    grub = root / "grub"
    grub.mkdir()
    (grub / "module.mod").write_bytes(b"module")
    (grub / "grub-bios-setup").write_bytes(b"setup")
    program = root / "program"
    program.write_bytes(b"program")
    (root / "out/coreutils-applets").write_text('[\nls\nclear\nhostname\n')
    for name in ("bash", "mount", "umount", "setsid", "fdisk", "blockdev", "dmesg", "ps", "grep", "sed", "find", "clear", "reset", "mkfs.ext4", "wpa_supplicant"):
        (root / "out" / name).write_bytes(name.encode())
    library = root / "libfixture.so"
    library.write_bytes(b"library")
    loader = root / "ld-fixture.so"
    loader.write_bytes(b"loader")
    args = [str(SCRIPT), str(root), str(grub), *([str(program)] * 3)]
    ldd = f"libfixture.so => {library} (0x0)\n{loader} (0x0)\n"
    return args, ldd


class RootfsTests(unittest.TestCase):
    def test_windows_checkout_packages_valid_shell_scripts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            args, ldd = make_fixture(root)
            # Package a real CRLF checkout, including the extensionless init.
            tooling = root / "tooling"
            tooling.mkdir()
            script_path = tooling / "mkrootfs.py"
            script_path.write_bytes(SCRIPT.read_bytes())
            for name in ("init", "bash.bashrc", "inputrc"):
                data = SCRIPT.with_name(name).read_bytes()
                (tooling / name).write_bytes(data.replace(b"\r\n", b"\n").replace(b"\n", b"\r\n"))
            example = root / "HOSWM/examples/hello_gui.c"
            example.parent.mkdir(parents=True)
            example.write_bytes(b"/* fixture */\n")
            args[0] = str(script_path)
            with patch.object(sys, "argv", args), \
                 patch("subprocess.check_output", return_value=ldd), \
                 patch("shutil.which", side_effect=lambda name: str(root / "out" / name)), \
                 contextlib.redirect_stdout(io.StringIO()):
                runpy.run_path(str(script_path), run_name="__main__")
            entries = read_archive(root / "out/initramfs.cpio.gz")
            for name in ("hos-live-start", "bash.bashrc", "inputrc"):
                self.assertNotIn(b"\r", entries[f"etc/{name}"][1])
            script = entries["etc/hos-live-start"][1]
            self.assertTrue(script.startswith(b"#!/bin/sh\n"))
            self.assertEqual(entries["etc/hos-live-start"][0][1], stat.S_IFREG | 0o755)
            subprocess.run(["bash", "-n"], input=script, check=True)

    def test_guest_metadata_does_not_require_host_permissions(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            args, ldd = make_fixture(root)
            with patch.object(sys, "argv", args), \
                 patch("subprocess.check_output", return_value=ldd), \
                 patch("shutil.which", side_effect=lambda name: str(root / "out" / name)), \
                 patch("os.chmod", side_effect=PermissionError("unsupported")), \
                 patch("os.symlink", side_effect=PermissionError("unsupported")), \
                 contextlib.redirect_stdout(io.StringIO()):
                runpy.run_path(str(SCRIPT), run_name="__main__")
                output = root / "out/initramfs.cpio.gz"
                first = output.read_bytes()
                runpy.run_path(str(SCRIPT), run_name="__main__")
                self.assertEqual(first, output.read_bytes())
                # A build host without wpa_supplicant can leave Wi-Fi out.
                with patch.dict("os.environ", {"HOS_WIFI": "0"}):
                    runpy.run_path(str(SCRIPT), run_name="__main__")
                    self.assertNotIn("sbin/wpa_supplicant", read_archive(output))
                # HOS_WPA_SUPPLICANT names the binary to bundle.
                chosen = root / "out/other-supplicant"
                chosen.write_bytes(b"chosen supplicant")
                with patch.dict("os.environ", {"HOS_WPA_SUPPLICANT": str(chosen)}):
                    runpy.run_path(str(SCRIPT), run_name="__main__")
                    self.assertEqual(read_archive(output)["sbin/wpa_supplicant"][1], b"chosen supplicant")
                runpy.run_path(str(SCRIPT), run_name="__main__")
            entries = read_archive(output)
            self.assertNotIn("bin/hos-install", entries)
            self.assertIn("etc/hos-live", entries)
            for name in ("bin/coreutils", "bin/hos-password", "bin/hos-init", "bin/hoswm", "bin/hos-hello",
                         "bin/hos-terminal", "bin/hos-installer", "bin/hos-about", "bin/hos-files", "bin/hos-image", "bin/hos-notifications",
                         "bin/hos-toast", "bin/hos-account", "bin/hos-settings", "bin/hos-notepad", "bin/hos-snake",
                         "bin/hos-paint", "bin/hos-colorpicker",
                         "bin/hos-netd", "bin/hos-power", "bin/hos-ntpd", "bin/hos-soundd", "bin/hosctl",
                         "usr/sbin/grub-install", "usr/sbin/grub-probe", "usr/bin/grub-mkimage",
                         "usr/lib/grub/i386-pc/grub-bios-setup", "sbin/mkfs.ext4",
                         # Wi-Fi: hos-netd starts this and drives its socket.
                         "sbin/wpa_supplicant"):
                self.assertEqual(entries[name][0][1], stat.S_IFREG | 0o755)
            for app in ("useradd", "usermod", "userdel", "sudo"):
                self.assertEqual(entries[f"bin/{app}"][0][1], stat.S_IFLNK | 0o777)
                self.assertEqual(entries[f"bin/{app}"][1], b"hos-account")
            self.assertEqual(entries["init"][0][1], stat.S_IFLNK | 0o777)
            self.assertEqual(entries["init"][1], b"bin/hos-init")
            self.assertEqual(entries["bin/sh"][0][1], stat.S_IFLNK | 0o777)
            self.assertEqual(entries["bin/sh"][1], b"bash")
            # Applets are links to the coreutils binary, except where a host
            # program of the same name is bundled: clear and reset come from
            # the host so they match the packaged terminfo.
            self.assertEqual(entries["bin/ls"][1], b"coreutils")
            self.assertEqual(entries["bin/clear"][0][1], stat.S_IFREG | 0o755)
            self.assertEqual(entries["sbin/init"][1], b"../bin/hos-init")
            self.assertEqual(entries["tmp"][0][1], stat.S_IFDIR | 0o1777)
            # The init system needs these before its services start.
            for name in ("run/hos", "etc/hos", "var/lib/hos"):
                self.assertEqual(entries[name][0][1], stat.S_IFDIR | 0o755)
            self.assertEqual(entries["root"][0][1], stat.S_IFDIR | 0o700)
            self.assertEqual(entries["usr/lib/grub/i386-pc/module.mod"][0][1], stat.S_IFREG | 0o644)
            self.assertEqual(entries["lib/libfixture.so"][1], b"library")
            self.assertEqual(entries["lib64/ld-fixture.so"][1], b"loader")
            self.assertEqual(entries["dev/console"][0][1], stat.S_IFCHR | 0o600)
            self.assertEqual(entries["dev/console"][0][9:11], [5, 1])
            self.assertEqual(entries["usr/include/hoswm.h"][1], b"hoswm.h")
            self.assertEqual(entries["usr/lib/libhoswm.a"][1], b"libhoswm.a")
            self.assertIn(b"hos_gui_window_create", entries["usr/share/hoswm/hello_gui.c"][1])
            self.assertIn(b"mount -t devpts", entries["etc/hos-live-start"][1])
            for fields, _ in entries.values():
                self.assertEqual(fields[2:4], [0, 0])

    def test_static_programs_carry_no_libraries(self):
        # A wpa_supplicant built from source is statically linked, and ldd
        # answers for such a program by failing instead of listing libraries.
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            args, _ = make_fixture(root)
            with patch.object(sys, "argv", args), \
                 patch("subprocess.check_output", side_effect=subprocess.CalledProcessError(1, "ldd")), \
                 patch("shutil.which", side_effect=lambda name: str(root / "out" / name)), \
                 contextlib.redirect_stdout(io.StringIO()):
                runpy.run_path(str(SCRIPT), run_name="__main__")
            entries = read_archive(root / "out/initramfs.cpio.gz")
            self.assertEqual(entries["sbin/wpa_supplicant"][0][1], stat.S_IFREG | 0o755)
            self.assertEqual(entries["sbin/wpa_supplicant"][1], b"wpa_supplicant")
            self.assertFalse([name for name in entries if name.startswith(("lib/", "lib64/"))])


if __name__ == "__main__":
    unittest.main()
