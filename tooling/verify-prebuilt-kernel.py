#!/usr/bin/env python3
"""Reject kernel images without the exact x86_64 boot/device config we need."""
import pathlib, struct, sys

image = pathlib.Path(sys.argv[1]).read_bytes()
if len(image) < 0x210 or struct.unpack_from("<H", image, 0x1FE)[0] != 0xAA55 or image[0x202:0x206] != b"HdrS":
    raise SystemExit("ERROR: kernel file is not an x86 Linux bzImage/vmlinuz")
config = pathlib.Path(sys.argv[2]).read_text(errors="replace").splitlines()
enabled = {line.removeprefix("CONFIG_").split("=", 1)[0] for line in config if line.startswith("CONFIG_") and line.endswith("=y")}
required = """64BIT BLK_DEV_INITRD BLK_DEV DEVTMPFS DEVTMPFS_MOUNT PROC_FS SYSFS TTY UNIX98_PTYS UNIX NET INET PACKET NETDEVICES ETHERNET VIRTIO_NET E1000 E1000E R8169 PCNET32 CFG80211 MAC80211 WLAN SERIAL_8250 SERIAL_8250_CONSOLE PCI VIRTIO_MENU VIRTIO_PCI VIRTIO_BLK SCSI SCSI_LOWLEVEL SCSI_VIRTIO ISO9660_FS BLK_DEV_SR BLK_DEV_SD DRM DRM_VIRTIO_GPU DRM_VIRTIO_GPU_KMS INPUT INPUT_EVDEV INPUT_KEYBOARD KEYBOARD_ATKBD INPUT_MOUSE MOUSE_PS2 SERIO_I8042 VT VT_CONSOLE FB FB_DEVICE FB_VESA EFI FB_EFI FRAMEBUFFER_CONSOLE VGA_CONSOLE DRM_FBDEV_EMULATION DRM_BOCHS DRM_VMWGFX DRM_VBOXVIDEO ATA_PIIX SATA_AHCI USB USB_STORAGE USB_UAS USB_HID USB_XHCI_PCI USB_EHCI_PCI USB_OHCI_HCD_PCI USB_UHCI_HCD EXT4_FS""".split()
missing = sorted(set(required) - enabled)
if missing:
    raise SystemExit("ERROR: kernel config lacks built-in required options: " + ", ".join(missing))
print(f"Verified x86_64 bzImage ({len(image)} bytes) and required built-in options")
