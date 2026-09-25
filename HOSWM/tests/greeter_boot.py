#!/usr/bin/env python3
"""Graphical login VM test (no host accounts are changed).
Usage: python3 tests/greeter_boot.py VMLINUZ BUSYBOX HOSWM
Tests wrong passwords, a non-root session, supplementary groups and logout/root login.
Requires QEMU, gcc, mkfs.ext4, sfdisk and debugfs.
"""
from pathlib import Path
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from installed_boot import run


def test(kernel, busybox, hoswm):
    with tempfile.TemporaryDirectory(prefix="hos-greeter-") as directory:
        base = Path(directory)
        root = base / "root"
        for name in ["bin", "sbin", "etc/init.d", "dev", "proc", "sys", "tmp", "root", "home/alice"]:
            (root / name).mkdir(parents=True)
        (root / "tmp").chmod(0o1777)
        (root / "root").chmod(0o700)
        for program, name in [(busybox, "busybox"), (hoswm, "hoswm")]:
            shutil.copy2(program, root / "bin" / name)
        for name in ["sh", "mount", "mkdir", "hostname", "cat", "echo", "sleep"]:
            (root / "bin" / name).symlink_to("busybox")
        (root / "sbin/init").symlink_to("../bin/busybox")
        shutil.copyfile(Path(__file__).resolve().parents[1] / "src/installed_rcS.sh", root / "etc/init.d/rcS")
        (root / "etc/init.d/rcS").chmod(0o755)
        (root / "etc/hostname").write_text("hos-greeter-test\n")
        (root / "etc/passwd").write_text("root:x:0:0:root:/root:/bin/sh\nalice:x:1000:100:Alice:/home/alice:/bin/sh\n")
        hash_value = run(str(busybox), "cryptpw", "-m", "sha512", "-S", "test-salt", "-P", "0",
                         input="testpass\n", text=True, stdout=subprocess.PIPE).stdout.strip()
        (root / "etc/shadow").write_text("".join(f"{name}:{hash_value}:19000:0:99999:7:::\n" for name in ["root", "alice"]))
        (root / "etc/shadow").chmod(0o600)
        (root / "etc/group").write_text("root:x:0:\nusers:x:100:alice\nextra:x:200:alice\n")
        drm = os.environ.get("HOS_TEST_DRM") == "1"
        command = "/bin/hoswm" if drm else "/bin/busybox env HOS_FB_DEVICE=/dev/fb0 /bin/hoswm"
        (root / "bin/session").write_text(f"#!/bin/sh\nexec {command} --greeter 2>/dev/ttyS0\n")
        (root / "bin/session").chmod(0o755)
        (root / "etc/inittab").write_text("::sysinit:/etc/init.d/rcS\ntty1::respawn:/bin/session\n::once:/bin/watch-test\n")
        (root / "bin/prove-session").write_text("""#!/bin/sh
{
    echo "$USER:$LOGNAME:$HOME:$SHELL"
    pwd
    cat /proc/$$/status
    if [ "$USER" = alice ]; then
        if echo bad > /root/forbidden; then echo PRIVILEGE_LEAK; fi
        if cat /etc/shadow; then echo SHADOW_LEAK; fi
    fi
} > "$HOME/proof"
""")
        (root / "bin/prove-session").chmod(0o755)
        (root / "bin/watch-test").write_text("""#!/bin/sh
echo GREETER_BOOT_READY
while [ ! -S /tmp/hoswm-1000/session.sock ]; do sleep 1; done
echo ALICE_SESSION_STARTED
while [ ! -f /home/alice/proof ]; do sleep 1; done
sleep 1
cat /home/alice/proof
echo ALICE_PROOF_DONE
while [ ! -f /root/proof ]; do sleep 1; done
sleep 1
cat /root/proof
echo ROOT_PROOF_DONE
/bin/busybox poweroff -f
""")
        (root / "bin/watch-test").chmod(0o755)
        filesystem = base / "ext4.img"
        with filesystem.open("wb") as f:
            f.truncate(96 * 1024 * 1024)
        run("mkfs.ext4", "-q", "-F", "-L", "HOSROOT", "-d", str(root), str(filesystem))
        # mkfs -d preserves host ownership: explicitly model installed ownership.
        commands = []
        for entry in root.rglob("*"):
            name = "/" + str(entry.relative_to(root))
            uid, gid = (1000, 100) if name == "/home/alice" else (0, 0)
            commands += [f"set_inode_field {name} uid {uid}", f"set_inode_field {name} gid {gid}"]
        fix = base / "ownership.debugfs"
        fix.write_text("\n".join(commands) + "\n")
        run("debugfs", "-w", "-f", str(fix), str(filesystem), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        disk = base / "disk.img"
        with disk.open("wb") as f:
            f.truncate(100 * 1024 * 1024)
        run("sfdisk", str(disk), input="label: dos\nstart=2048, size=196608, type=83, bootable\n", text=True, stdout=subprocess.DEVNULL)
        with disk.open("r+b") as dst, filesystem.open("rb") as src:
            dst.seek(1048576)
            shutil.copyfileobj(src, dst)
        serial = base / "serial.log"
        vm = subprocess.Popen(["qemu-system-x86_64", "-m", "256", "-display", "none", "-vga", "virtio" if drm else "std",
                               "-serial", f"file:{serial}", "-qmp", "stdio", "-no-reboot",
                               "-kernel", str(kernel), "-drive", f"file={disk},format=raw,if=ide",
                               "-append", "root=/dev/sda1 rootfstype=ext4 rootwait ro console=tty0 console=ttyS0,115200n8 panic=1"],
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        def log():
            return serial.read_text(errors="replace") if serial.exists() else ""
        def qmp(command, arguments=None):
            vm.stdin.write(json.dumps({"execute": command, "arguments": arguments or {}}) + "\n")
            vm.stdin.flush()
            while True:
                line = vm.stdout.readline()
                assert line, log()
                reply = json.loads(line)
                assert "error" not in reply, reply
                if "return" in reply:
                    return reply["return"]
        def key(value):
            qmp("human-monitor-command", {"command-line": f"sendkey {value} 30"})
            time.sleep(0.075)
        def type_text(value):
            for char in value:
                key({"\t": "tab", "\n": "ret", " ": "spc", "-": "minus", "/": "slash"}.get(char, char))
        def mouse_move(dx, dy):
            qmp("input-send-event", {"events": [
                {"type": "rel", "data": {"axis": "x", "value": dx}},
                {"type": "rel", "data": {"axis": "y", "value": dy}},
            ]})
            time.sleep(0.1)
        def mouse_button(down):
            qmp("input-send-event", {"events": [{"type": "btn", "data": {"button": "left", "down": down}}]})
            time.sleep(0.1)
        def screen_pixel(x, y):
            dump = base / "screen.ppm"
            qmp("screendump", {"filename": str(dump)})
            magic, size, maximum, pixels = dump.read_bytes().split(b"\n", 3)
            assert magic == b"P6" and maximum == b"255"
            width, height = map(int, size.split())
            w, h = (width, width * 600 // 800) if width * 600 <= height * 800 else (height * 800 // 600, height)
            px = (width - w) // 2 + (2 * x + 1) * w // 1600
            py = (height - h) // 2 + (2 * y + 1) * h // 1200
            offset = (py * width + px) * 3
            return pixels[offset:offset + 3]
        def wait_for(marker, timeout=15):
            deadline = time.monotonic() + timeout
            while marker not in log() and time.monotonic() < deadline:
                if vm.poll() is not None:
                    break
                time.sleep(0.1)
            assert marker in log(), log()
        try:
            qmp("qmp_capabilities")
            wait_for("HOSWM DRM ready" if drm else "HOSWM framebuffer ready")
            time.sleep(0.5)
            type_text("alice\twrong\n")
            time.sleep(2.2)
            assert "ALICE_SESSION_STARTED" not in log(), log()
            type_text("testpass\n")
            time.sleep(1)
            type_text("prove-session\n")
            wait_for("ALICE_PROOF_DONE")
            if drm:
                assert "HOSWM DRM hardware cursor enabled" in log(), log()
                assert "HOSWM DRM page flips active" in log(), log()
                assert "page flips unavailable" not in log(), log()
            alice = log().split("ALICE_PROOF_DONE")[0]
            assert "alice:alice:/home/alice:/bin/sh" in alice, alice
            assert "Uid:\t1000\t1000\t1000\t1000" in alice, alice
            assert "Gid:\t100\t100\t100\t100" in alice, alice
            assert "Groups:\t100 200" in alice, alice
            assert "PRIVILEGE_LEAK" not in alice and "SHADOW_LEAK" not in alice, alice
            # Drag the terminal and verify both its new border and the restored
            # background through actual scanout, including alternating DRM buffers.
            time.sleep(0.1)
            assert screen_pixel(60, 40) == bytes([114, 219, 172])
            # PS/2 packets have bounded deltas; reset with multiple reports.
            for _ in range(10):
                mouse_move(-100, -100)
            mouse_move(100, 50)
            mouse_button(True)
            mouse_move(70, 20)
            mouse_button(False)
            old_border = screen_pixel(60, 40)
            new_border = screen_pixel(130, 60)
            assert old_border == bytes([0, 0, 0]), (old_border, new_border)
            assert new_border == bytes([114, 219, 172]), (old_border, new_border)
            key("ctrl-alt-esc")
            time.sleep(2)
            type_text("root\ttestpass\n")
            time.sleep(1)
            type_text("prove-session\n")
            wait_for("ROOT_PROOF_DONE")
            assert "root:root:/root:/bin/sh" in log(), log()
            assert "Uid:\t0\t0\t0\t0" in log(), log()
            assert "HOSWM ERROR" not in log(), log()
            vm.wait(timeout=10)
            print(f"PASS ({'DRM' if drm else 'fbdev'}): login, keyboard, mouse drag/scanout, UID/GID/groups/home, permissions, logout and root login")
        finally:
            if vm.poll() is None:
                vm.terminate()
                vm.wait(timeout=5)


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit(__doc__)
    test(*(Path(arg).resolve() for arg in sys.argv[1:]))
