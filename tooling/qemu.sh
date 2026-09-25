#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/env.sh"
need qemu-system-x86_64
mode=${1:-live}
if [[ "$mode" == live ]]; then
    [[ -f "$BUILD/out/hackerOS.iso" ]] || fail 'Build the ISO first: tooling/build.sh all'
    exec qemu-system-x86_64 -m 1024 -vga virtio -nic user,model=virtio-net-pci -device virtio-scsi-pci,id=scsi0 -drive "file=$BUILD/out/hackerOS.iso,media=cdrom,if=none,id=installer" -device scsi-cd,drive=installer,bus=scsi0.0 -drive "file=$BUILD/hOS-install.qcow2,if=virtio,format=qcow2" -boot d -serial stdio
elif [[ "$mode" == disk ]]; then
    [[ -f "$BUILD/hOS-install.qcow2" ]] || fail 'Create the virtual disk first: tooling/qemu.sh create-disk'
    exec qemu-system-x86_64 -m 1024 -vga virtio -nic user,model=virtio-net-pci -drive "file=$BUILD/hOS-install.qcow2,if=virtio,format=qcow2" -boot c -serial stdio
elif [[ "$mode" == create-disk ]]; then
    need qemu-img
    qemu-img create -f qcow2 "$BUILD/hOS-install.qcow2" 2G
    echo "Created $BUILD/hOS-install.qcow2"
else
    fail 'Usage: tooling/qemu.sh [live|disk|create-disk]'
fi
