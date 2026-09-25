#!/bin/sh
export PATH=/bin:/sbin:/usr/bin:/usr/sbin
# Mount proc before resolving the root remount.
mount -t proc proc /proc
mount -o remount,rw /
# The kernel mounts devtmpfs already when booting directly from disk.
if ! /bin/grep -q ' /dev devtmpfs ' /proc/mounts; then
    mount -t devtmpfs devtmpfs /dev
fi
mount -t sysfs sysfs /sys
mkdir -p /dev/pts
mount -t devpts devpts /dev/pts
mkdir -p /dev/dri /dev/input
# Runtime state for the init system's services lives on a tmpfs, so their
# sockets never survive a reboot.
mkdir -p /run
mount -t tmpfs tmpfs /run
mkdir -p /run/hos /etc/hos /var/lib/hos
hostname "$(cat /etc/hostname)"
