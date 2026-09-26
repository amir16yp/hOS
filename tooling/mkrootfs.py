#!/usr/bin/env python3
"""Assemble a deterministic newc initramfs without host filesystem metadata."""
import gzip
import os
import pathlib
import shutil
import stat
import subprocess
import sys

build = pathlib.Path(sys.argv[1])
# Record guest metadata directly: Windows mounts may reject chmod and may
# report every host file as executable. No on-disk rootfs staging is needed.
entries = {".": (stat.S_IFDIR | 0o755, b"")}


def add(name, mode, data=b""):
    parent = pathlib.PurePosixPath(name).parent
    if str(parent) != "." and str(parent) not in entries:
        add(str(parent), stat.S_IFDIR | 0o755)
    entries[name] = (mode, data)


def copy_file(src, name, permissions=0o644):
    add(name, stat.S_IFREG | permissions, src.read_bytes())


def copy_text(src, name, permissions=0o644):
    # Normalize Windows checkouts before packaging Linux shell/config files.
    add(name, stat.S_IFREG | permissions, src.read_text().encode())


def copy_tree(src, name):
    if src.is_symlink():
        add(name, stat.S_IFLNK | 0o777, os.readlink(src).encode())
    elif src.is_dir():
        add(name, stat.S_IFDIR | 0o755)
        for child in sorted(src.iterdir()):
            copy_tree(child, f"{name}/{child.name}")
    else:
        copy_file(src, name)


def copy_program(src, name):
    copy_file(src, name, 0o755)
    # Resolve shared libraries at build time so the guest is self-contained.
    # A statically linked program has none, which ldd reports as a failure.
    try:
        listing = subprocess.check_output(["ldd", str(src)], text=True, stderr=subprocess.DEVNULL)
    except subprocess.CalledProcessError:
        return
    for line in listing.splitlines():
        if "=>" in line and "/" in line.split("=>", 1)[1]:
            soname = line.strip().split("=>", 1)[0].strip()
            resolved = pathlib.Path(line.split("=>", 1)[1].strip().split()[0])
            copy_file(resolved.resolve(), f"lib/{soname}", 0o755)
        elif line.lstrip().startswith("/") and "(" in line:
            resolved = pathlib.Path(line.strip().split()[0])
            copy_file(resolved.resolve(), f"lib64/{resolved.name}", 0o755)


for name in ("bin", "sbin", "dev", "proc", "sys", "tmp", "etc", "root",
             # The init system's runtime sockets, configuration and state.
             "run", "run/hos", "etc/hos", "var", "var/lib", "var/lib/hos"):
    add(name, stat.S_IFDIR | (0o1777 if name == "tmp" else 0o700 if name == "root" else 0o755))
for src, name in ((build / "out/coreutils", "bin/coreutils"),
                  (build / "out/hos-password", "bin/hos-password"),
                  (build / "out/hos-init", "bin/hos-init"),
                  (build / "out/hoswm", "bin/hoswm"),
                  (build / "out/hos-hello", "bin/hos-hello"),
                  (build / "out/hos-terminal", "bin/hos-terminal"),
                  (build / "out/hos-installer", "bin/hos-installer"),
                  (build / "out/hos-about", "bin/hos-about"),
                  (build / "out/hos-files", "bin/hos-files"),
                  (build / "out/hos-image", "bin/hos-image"),
                  (build / "out/hos-paint", "bin/hos-paint"),
                  (build / "out/hos-colorpicker", "bin/hos-colorpicker"),
                  (build / "out/hos-notifications", "bin/hos-notifications"),
                  (build / "out/hos-toast", "bin/hos-toast"),
                  (build / "out/hos-account", "bin/hos-account"),
                  (build / "out/hos-settings", "bin/hos-settings"),
                  (build / "out/hos-notepad", "bin/hos-notepad"),
                  (build / "out/hos-snake", "bin/hos-snake"),
                  # The init system's services and its command-line client.
                  (build / "out/hos-netd", "bin/hos-netd"),
                  (build / "out/hos-power", "bin/hos-power"),
                  (build / "out/hos-ntpd", "bin/hos-ntpd"),
                  (build / "out/hos-soundd", "bin/hos-soundd"),
                  (build / "out/hosctl", "bin/hosctl")):
    copy_file(src, name, 0o755)
copy_text(pathlib.Path(__file__).with_name("init"), "etc/hos-live-start", 0o755)
for app in ("useradd", "usermod", "userdel", "sudo"):
    add(f"bin/{app}", stat.S_IFLNK | 0o777, b"hos-account")
# Keep a copy of the running kernel in the RAM-backed live root. The installer
# needs it after booting from USB, where no optical drive contains the ISO.
copy_file(build / "out/vmlinuz", "boot/vmlinuz")
add("etc/hos-live", stat.S_IFREG | 0o644, b"hOS live installer\n")
copy_file(build / "out/hoswm.h", "usr/include/hoswm.h")
copy_file(build / "out/libhoswm.a", "usr/lib/libhoswm.a")
copy_file(pathlib.Path(__file__).resolve().parents[1] / "HOSWM/examples/hello_gui.c", "usr/share/hoswm/hello_gui.c")
# The applet list comes from the checksum-verified release itself.
applet_lines = (build / "out/coreutils-applets").read_text().splitlines()
if len(applet_lines) < 2 or applet_lines[0] != "[":
    raise SystemExit("Invalid uutils applet list")
for name in (line.strip() for line in applet_lines[1:]):
    if not name or "/" in name or name in (".", "..", "coreutils"):
        raise SystemExit(f"Invalid coreutils applet: {name!r}")
    add(f"bin/{name}", stat.S_IFLNK | 0o777, b"coreutils")
copy_file(build / "out/coreutils-LICENSE", "usr/share/licenses/uutils/LICENSE")
# These programs cover the system services outside coreutils' scope.
for name in ("bash", "mount", "umount", "setsid", "fdisk", "blockdev", "dmesg", "ps", "grep", "sed", "find", "clear", "reset"):
    program = shutil.which(name)
    if not program:
        raise SystemExit(f"ERROR: required userspace program missing: {name}")
    copy_program(pathlib.Path(program).resolve(), f"bin/{name}")
add("bin/sh", stat.S_IFLNK | 0o777, b"bash")
# Wi-Fi: hos-netd starts wpa_supplicant and drives its control socket. It
# lives in sbin, which is not always on a user's PATH, and HOS_WIFI=0 leaves
# it out for build hosts that have none.
if os.environ.get("HOS_WIFI", "1") != "0":
    supplicant = os.environ.get("HOS_WPA_SUPPLICANT") or shutil.which("wpa_supplicant")
    if not supplicant:
        for directory in ("/usr/local/sbin", "/usr/sbin", "/sbin"):
            candidate = pathlib.Path(directory) / "wpa_supplicant"
            if candidate.is_file():
                supplicant = candidate
                break
    if not supplicant:
        raise SystemExit("ERROR: wpa_supplicant is bundled for Wi-Fi; install it, set "
                         "HOS_WPA_SUPPLICANT, or build without Wi-Fi using HOS_WIFI=0")
    copy_program(pathlib.Path(supplicant).resolve(), "sbin/wpa_supplicant")
for name in ("reboot", "poweroff", "halt"):
    add(f"bin/{name}", stat.S_IFLNK | 0o777, b"hos-init")
add("init", stat.S_IFLNK | 0o777, b"bin/hos-init")
add("sbin/init", stat.S_IFLNK | 0o777, b"../bin/hos-init")
for name in ("bash.bashrc", "inputrc"):
    copy_text(pathlib.Path(__file__).with_name(name), f"etc/{name}")
add("etc/profile", stat.S_IFREG | 0o644, b'export PATH=/bin:/sbin:/usr/bin:/usr/sbin\n[ -n "$BASH_VERSION" ] && . /etc/bash.bashrc\n')
add("etc/passwd", stat.S_IFREG | 0o644, b"root:x:0:0:root:/root:/bin/bash\n")
add("etc/group", stat.S_IFREG | 0o644, b"root:x:0:\n")
add("etc/shadow", stat.S_IFREG | 0o600, b"root:!:19000:0:99999:7:::\n")
# Terminfo lets clear/reset and Readline use the same terminal capabilities.
for name in ("ansi", "linux", "xterm", "xterm-256color"):
    for base in (pathlib.Path("/usr/share/terminfo"), pathlib.Path("/lib/terminfo")):
        source = base / name[0] / name
        if source.is_file():
            copy_file(source.resolve(), f"usr/share/terminfo/{name[0]}/{name}")
            break
    else:
        raise SystemExit(f"ERROR: missing terminfo entry: {name}")

# Bundle the pinned host GRUB runtime needed by the install command.
grub_dir = pathlib.Path(sys.argv[2])
install_bin, image_bin, probe_bin = map(pathlib.Path, sys.argv[3:6])
copy_tree(grub_dir, "usr/lib/grub/i386-pc")
for src, name in ((install_bin, "usr/sbin/grub-install"),
                  (probe_bin, "usr/sbin/grub-probe"),
                  (image_bin, "usr/bin/grub-mkimage"),
                  (grub_dir / "grub-bios-setup", "usr/lib/grub/i386-pc/grub-bios-setup")):
    copy_program(src, name)

# The installer formats ext4 using e2fsprogs.
mkfs_ext4 = shutil.which("mkfs.ext4")
if not mkfs_ext4:
    raise SystemExit("ERROR: mkfs.ext4 is required to build the ext4 installer")
copy_program(pathlib.Path(mkfs_ext4).resolve(), "sbin/mkfs.ext4")

out = bytearray()
ino = 1


def append_entry(name, mode, data=b"", major=0, minor=0):
    global ino
    name = name.encode() + b"\0"
    fields = (ino, mode, 0, 0, 1, 0, len(data), 0, 0, major, minor, len(name), 0)
    out.extend(b"070701" + b"".join(f"{v:08x}".encode() for v in fields) + name)
    out.extend(b"\0" * ((-len(out)) % 4))
    out.extend(data)
    out.extend(b"\0" * ((-len(out)) % 4))
    ino += 1


for name, (mode, data) in sorted(entries.items()):
    append_entry(name, mode, data)
for name, (major, minor) in {"dev/console": (5, 1), "dev/null": (1, 3), "dev/tty": (5, 0), "dev/zero": (1, 5)}.items():
    append_entry(name, stat.S_IFCHR | 0o600, major=major, minor=minor)
append_entry("TRAILER!!!", 0)
dst = build / "out/initramfs.cpio.gz"
with dst.open("wb") as f:
    with gzip.GzipFile(filename="", mode="wb", fileobj=f, mtime=int(os.environ.get("SOURCE_DATE_EPOCH", "0"))) as gz:
        gz.write(out)
print(f"Created {dst} ({len(out)} bytes unpacked)")
