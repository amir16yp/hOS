#!/usr/bin/env python3
"""Boot the installed rcS on a read-only ext4 root and check GUI prerequisites.

Usage: python3 tests/installed_boot.py /path/to/vmlinuz /path/to/busybox
Requires QEMU, gcc (static libc), mkfs.ext4 and sfdisk. Uses temporary disk images.
"""
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def check_boot(kernel, busybox):
    with tempfile.TemporaryDirectory(prefix="hos-installed-boot-") as directory:
        base = Path(directory)
        root = base / "root"
        for name in ["bin", "sbin", "etc/init.d", "dev", "proc", "sys", "tmp"]:
            (root / name).mkdir(parents=True)
        (root / "tmp").chmod(0o1777)
        shutil.copy2(busybox, root / "bin/busybox")
        for name in ["sh", "mount", "mkdir", "hostname"]:
            (root / "bin" / name).symlink_to("busybox")
        (root / "sbin/init").symlink_to("../bin/busybox")
        shutil.copyfile(Path(__file__).resolve().parents[1] / "src/installed_rcS.sh",
                        root / "etc/init.d/rcS")
        (root / "etc/init.d/rcS").chmod(0o755)
        (root / "etc/hostname").write_text("hos-boot-test\n")
        (root / "etc/inittab").write_text(
            "::sysinit:/etc/init.d/rcS\n::once:/bin/boot-check\n")
        # Exercise the same filesystem and Unix socket operations as ABI startup.
        source = base / "socket-check.c"
        source.write_text(r'''
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <stdio.h>
#include <unistd.h>
int main(void) {
    if (mkdir("/tmp/hoswm-0", 0700)) { perror("mkdir"); return 1; }
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    struct sockaddr_un addr = {.sun_family = AF_UNIX,
        .sun_path = "/tmp/hoswm-0/session.sock"};
    if (fd < 0 || bind(fd, (struct sockaddr *)&addr, sizeof(addr)) ||
        chmod(addr.sun_path, 0600) || listen(fd, 1)) {
        perror("socket startup"); return 1;
    }
    close(fd);
    return 0;
}
''')
        run("gcc", "-static", "-Wall", "-Wextra", "-Werror", str(source),
            "-o", str(root / "bin/socket-check"))
        (root / "bin/boot-check").write_text("""#!/bin/sh
if /bin/busybox grep -q ' / ext4 rw,' /proc/mounts &&
   /bin/busybox grep -q ' /dev/pts devpts ' /proc/mounts &&
   [ "$(hostname)" = hos-boot-test ] &&
   echo writable > /etc/write-check && /bin/socket-check; then
    echo HOS_INSTALLED_BOOT_OK
else
    echo HOS_INSTALLED_BOOT_FAILED
fi
/bin/busybox poweroff -f
""")
        (root / "bin/boot-check").chmod(0o755)
        filesystem = base / "ext4.img"
        with filesystem.open("wb") as stream:
            stream.truncate(64 * 1024 * 1024)
        run("mkfs.ext4", "-q", "-F", "-L", "HOSROOT", "-d", str(root), str(filesystem))
        disk = base / "disk.img"
        with disk.open("wb") as stream:
            stream.truncate(68 * 1024 * 1024)
        run("sfdisk", str(disk), input="label: dos\nstart=2048, size=131072, type=83, bootable\n",
            text=True, stdout=subprocess.DEVNULL)
        with disk.open("r+b") as dest, filesystem.open("rb") as source:
            dest.seek(1048576)
            shutil.copyfileobj(source, dest)
        result = run("qemu-system-x86_64", "-m", "256", "-display", "none",
                     "-serial", "stdio", "-monitor", "none", "-no-reboot",
                     "-kernel", str(kernel), "-drive", f"file={disk},format=raw,if=ide",
                     "-append", "root=/dev/sda1 rootfstype=ext4 rootwait ro console=ttyS0,115200n8 panic=1",
                     stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=45)
        log = result.stdout
        assert "HOS_INSTALLED_BOOT_OK" in log, log
        for error in ["Read-only file system", "Device or resource busy", "can't read '/proc/mounts'", "Kernel panic"]:
            assert error not in log, log
        print("PASS: installed BusyBox init/rcS remounts root writable, mounts devpts, sets hostname and starts a Unix socket")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    check_boot(Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve())
