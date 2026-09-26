
## Screenshots
<img width="1281" height="878" alt="image" src="https://github.com/user-attachments/assets/826658a1-b1d7-44d1-ac22-794c7f167fda" />
<img width="1281" height="878" alt="image" src="https://github.com/user-attachments/assets/d28c894c-1f1c-4a00-8753-07385c70fa57" />
<img width="1281" height="878" alt="image" src="https://github.com/user-attachments/assets/52ec88fd-dbbd-4aa4-9e14-0e28fef0ce3e" />
<img width="1281" height="878" alt="image" src="https://github.com/user-attachments/assets/53bb5cbf-719d-4503-92e2-fff506d080ae" />



hOS is a custom x86_64 Linux-based operating system project with a Bash and uutils userspace, a BIOS-bootable live installer, and HOSWM: a Rust desktop and window server that draws directly to the Linux framebuffer. The current implementation includes movable windows, a shell terminal, bitmap text, and a C API for graphical applications. It runs without Xorg, Wayland, or a GPU-accelerated compositor.

This documentation describes the source tree as of September 25, 2026. It distinguishes implemented code from features that still need work; it is not a claim of successful boot testing on every supported device.

## Implemented so far

| Area | Current implementation |
| --- | --- |
| Boot | GRUB BIOS live ISO, graphical, largest-resolution, RAM-installer and diagnostic boot entries, self-contained Linux initramfs |
| System | Bash shell and uutils coreutils 0.12.0; native PID 1, password authentication, device/proc/sys mounts and PTYs |
| Services | `hos-init` supervises `hos-netd` (link state, DHCP, DNS, Wi-Fi through `wpa_supplicant`), `hos-power` (battery, power button, suspend), `hos-ntpd` (SNTP, asleep while offline) and `hos-soundd` (default device, volume, mute, mixing), each behind a line protocol on `/run/hos` |
| Sound | ALSA mixer controls without alsa-lib, a software mixer so several windows play at once, WAV playback, and a PCM API for applications |
| Desktop | Black background or a `~/.hoswm/wallpaper.qoi` wallpaper, screen-top menu bar with a clock, configurable dock, pointer, window focus and dragging, edge resizing for windows that ask for it, minimize, maximize/restore, close, message boxes, Alt+Tab window switching, on-screen notifications and QOI screenshots |
| Terminal | Standalone Rust `/bin/hos-terminal`, interactive Bash through a PTY, ANSI colors (16, 256 and true color), 2,000-line scrollback, resize, selectable text and sanitized bracketed paste |
| Graphics | Owned ARGB surfaces, clipped primitives, alpha compositing, PSF1 bitmap fonts |
| Applications | Unix-socket window ABI with Rust and C clients; standalone Rust terminal, file browser, image viewer, installer, settings, About, notification log, notification sender and account tools, GUI controls, message boxes with answer buttons and `hos-hello` demo |
| Settings | `hos-settings`, one window for the network, Wi-Fi, sound, power, time, desktop and service settings; `hosctl` does the same from a shell |
| Input | Hot-pluggable evdev keyboards, mice and tablets; devices may arrive and leave while the session runs |
| Configuration | `~/.hoswm/config.ini` for dock items and QOI icons, notification corner and log, screenshot directory, menu bar and key bindings |
| Installation | Standalone dock-launched Rust wizard, whole-disk BIOS/MBR installation, ext4 root partition, hostname and root password setup |
| Build | Pinned component versions, source checksum checks, kernel configuration validation, a static `wpa_supplicant` built from source when the host has none, output manifest |

For internals, see [architecture and source map](docs/architecture.md). For application development, see the [HOSWM API guide](docs/applications.md). For the init system, its services and their protocol, see [init and system services](docs/services.md). For session settings, see the [configuration guide](docs/configuration.md).

## Build host

Use an x86_64 Linux environment with network access for downloads. The scripts require:

- C/Rust tools: `gcc`, `cc`, `make`, `binutils` (including `ar` and `readelf`), and `cargo`/`rustc` supporting Rust edition 2024 and static x86_64 Linux builds.
- Kernel build tools: `flex`, `bison`, `m4`, `bc`, `perl`, and development libraries for ELF, zlib and zstd.
- Packaging tools: `python3`, `curl`, `tar`, `xz`, `bzip2`, `gzip`, `sha256sum`, `ldd`, `xorriso`, and `mkfs.ext4` from e2fsprogs.
- GRUB BIOS tools and `i386-pc` modules, normally provided by `grub-pc-bin` and `grub-common`. The build requires the pinned GRUB version.
- For VM use: `qemu-system-x86_64` and `qemu-img`.

A build host with no `wpa_supplicant` builds one from source instead, which additionally needs the Linux userspace headers (`linux-libc-dev`) and `strip`; `HOS_WIFI=0` skips Wi-Fi entirely.

The repository pins Linux **7.2.7**, uutils coreutils **0.12.0**, GRUB **2.14**, and — for that supplicant build — wpa_supplicant **2.12**, musl **1.2.6** and libnl **3.12.0** in [tooling/versions.env](tooling/versions.env). These are the build's configured versions. HOSWM is version **0.1.0**, uses Rust edition 2024, and currently has no external Rust dependencies.

## Build commands

Run from the project root:

```sh
tooling/build.sh all
```

Or build in stages:

```sh
tooling/build.sh fetch
tooling/build.sh kernel
tooling/build.sh userspace
tooling/build.sh hoswm
tooling/build.sh iso
```

| Stage | Purpose |
| --- | --- |
| `fetch` | Download and checksum the configured kernel and uutils release, plus the wpa_supplicant, musl and libnl sources when the host has no supplicant |
| `kernel` | Build and validate the kernel, or validate a configured prebuilt image |
| `userspace` | Package checksum-verified static uutils and build the password helper |
| `hoswm` | Build static HOSWM, its Rust applications and init, C client library/header, and static GUI demo |
| `iso` | Rebuild HOSWM and its SDK/demo, prefer the existing kernel with a prebuilt fallback, assemble the initramfs and BIOS ISO |
| `all` | Run the full sequence |

Outputs go under `.build/out/`: `vmlinuz`, `kernel.config`, `coreutils`, `hos-password`, `hos-init`, `hos-netd`, `hos-power`, `hos-ntpd`, `hos-soundd`, `hosctl`, `hoswm`, `hos-terminal`, `hos-files`, `hos-image`, `hos-installer`, `hos-settings`, `hos-about`, `hos-notifications`, `hos-toast`, `hos-account`, `hos-notepad`, `hos-snake`, `libhoswm.a`, `hoswm.h`, `hos-hello`, `initramfs.cpio.gz`, `hackerOS.iso`, and `build-manifest.txt`. The ISO keeps the `hackerOS.iso` filename. The manifest records configured pins, tool versions and output hashes. The RAM-backed initramfs contains the installer tools and a copy of the kernel, so installation does not depend on locating the boot media after startup. The initramfs writer encodes guest permissions directly, including on Windows-mounted filesystems.

After changing kernel configuration, rebuild `kernel` and `iso`. ISO packaging rebuilds HOSWM, but does not rebuild an existing kernel or userspace binaries.

ISO packaging first validates the kernel and matching config in `.build/out/`, published by a successful kernel build. If either file is missing or validation fails, it tries the configured prebuilt image regardless of `HOS_KERNEL_MODE`. The fallback must pass the same checks. ISO packaging does not use unfinished files from `.build/kernel/`.

| Variable | Use |
| --- | --- |
| `HOS_BUILD_DIR` | Relocate build sources, caches and outputs; use the same value for build and QEMU commands |
| `JOBS` | Parallel kernel build jobs; defaults to `nproc` |
| `HOS_GRUB_DIR` | Override the GRUB `i386-pc` module directory |
| `HOS_KERNEL_MODE` | `source` by default; optionally `prebuilt` |
| `HOS_WPA_SUPPLICANT` | Bundle this `wpa_supplicant` binary instead of searching the host or building one |
| `HOS_WIFI` | `0` leaves the supplicant out; Wi-Fi then reports that it is unavailable |
| `HOS_FB_DEVICE` | Runtime framebuffer path; defaults to `/dev/fb0` |

Prebuilt mode requires a compatible kernel and matching configuration. Override `HOS_PREBUILT_VMLINUZ_URL`, `HOS_PREBUILT_VMLINUZ_SHA256`, `HOS_PREBUILT_KERNEL_CONFIG_URL`, and `HOS_PREBUILT_KERNEL_CONFIG_SHA256` together. The legacy prebuilt defaults lack the required framebuffer support. Both kernel preparation and ISO packaging reject missing required built-in drivers.

## Boot and use

Create the 2 GiB development disk once, then boot the live ISO:

```sh
tooling/qemu.sh create-disk
tooling/qemu.sh live
```

`create-disk` is a provisioning command; keep an existing installed disk instead of recreating it. The QEMU wrapper uses 1 GiB RAM, virtio graphics, a virtio disk, a virtio-SCSI CD-ROM and serial output in the launching terminal.

The default GRUB entry uses a generic firmware framebuffer with `nomodeset`. The alternate native GPU entry allows kernel fbdev emulation. The third and fourth entries boot the same live system at the **largest resolution** the firmware or display offers instead of 800x600: GRUB takes the first mode it has from a list that starts at 3840x2160 and hands it to the kernel with `gfxpayload=keep`. The desktop itself is still drawn at 800x600 and scaled to fill the screen, so it looks larger, not more detailed. The native-GPU variant passes `hos.fbdev=1`, which keeps the session on the framebuffer device: the KMS path programs the display at 800x600 instead of using all of it. Choose **hOS RAM installer (USB / ISO)** when booting the installer from USB or when the media will not remain available after startup; GRUB loads the initramfs into memory, and the initramfs includes the kernel needed for installation. The diagnostics entry adds `hos.text=1` and opens a text shell without launching HOSWM. USB mass-storage and UAS drivers are built into the kernel, so USB flash drives and external disks can appear in the installer after the kernel enumerates them. IPv4, virtio-net, the common Intel, Realtek and AMD wired adapters, USB Ethernet and the 802.11 stack with ath9k are built in too, so the Network page has an interface to configure; adapters whose firmware is loaded from disk cannot work, because the image carries no firmware files.

The **Settings** dock icon opens `hos-settings`: pages for the network, Wi-Fi, sound, power, date and time, the desktop and the running services. It changes the running service and its file in `/etc/hos` at once, and edits `~/.hoswm/config.ini` for desktop settings, which apply when the session restarts. The same settings are reachable from a shell with `hosctl` — `hosctl` alone prints what every service is doing, `hosctl sound volume +5` changes one, and `hosctl watch net` follows events. Settings that need root are shown but refused when the session is not running as root.

Click the dock terminal icon to launch the standalone Rust terminal. The files, About, notifications and installer icons launch their own Rust applications through the HOSWM ABI. The file browser opens directories in tabs, copies and moves files and whole directories, opens a terminal in the directory it is showing from its menu or a right-click, and previews QOI images; opening one starts the image viewer, which zooms, pans and switches between smooth and nearest scaling. The dock comes from `~/.hoswm/config.ini`, which the session writes with the defaults on first start; each button may carry a QOI icon. Put a QOI image at `~/.hoswm/wallpaper.qoi` and the next session draws it behind the windows, scaled to cover the screen without distorting it, instead of the plain background; `[session] wallpaper` points somewhere else. Click windows to focus them, drag their title bars, and use the three right-hand title-bar controls to minimize, maximize/restore and close. Windows that declare themselves resizable, such as the terminal, carry a grip in the bottom-right corner and follow any edge or corner dragged with the pointer; the rest keep the size their application chose. The dock exit action returns to the console; **Ctrl+Alt+Escape** does the same, while Escape alone is passed to the focused application.

The menu bar across the top of the screen shows the focused window's name and menus, a **System** menu for screenshots and leaving the session, and a UTC clock. **Print** captures the screen and **Alt+Print** the focused window, as QOI images in `~/.hoswm/screenshots`. Notifications appear in a configured corner — for devices being plugged in or removed, screenshots, and anything `hos-toast "message"` sends — and are logged to `~/.hoswm/toastdb`, which the **Notifications** application lists. Click a notification to dismiss it.

**Alt+Tab** cycles through windows; add Shift to cycle in reverse, then release Alt to focus the highlighted window. Selectable text can be dragged over and copied with **Ctrl+C**; text controls also support Ctrl+X/V/A, and their right-click menu provides copy, cut, paste, delete and select-all actions. Clipboard data is shared by applications within the HOSWM session and does not connect to the host clipboard. In the terminal, use **Ctrl+Shift+C/V/A** so Ctrl+C remains available to the shell. Bash provides filename and command completion with Tab, shared persistent history, Ctrl+R search, and arrow-key history. The terminal keeps 2,000 lines of scrollback; hold Shift and press Page Up or Page Down to browse it. Multiline paste uses bracketed paste when the running program supports it.

Run the bundled graphical demo in a HOSWM terminal:

```sh
hos-hello
```

It displays a name textbox and a greeting button. Tab changes GUI control focus; Enter submits a textbox, and Enter or Space activates a focused button. The C API guide covers window events, selectable controls, clipboard behavior and the local socket protocol.

For VirtualBox, select the ISO printed by the build, BIOS boot with EFI disabled, at least 1 GiB RAM, VMSVGA or VBoxSVGA graphics, a PS/2 mouse, no 3D acceleration, and a network adapter of type Intel PRO/1000 or PCnet, both of which the kernel has drivers for. Start with the generic framebuffer entry. These are intended settings; hardware compatibility still needs validation in the target VM.

## Install to the virtual disk

Click **Install hOS** (the **HD** icon) in the HOSWM dock. The native wizard uses the same movable, minimizable window system as Terminal:

1. Select an unused writable disk (1 GiB to 2 TiB). Click Refresh after attaching a disk; arrow keys navigate longer disk lists.
2. Enter a hostname and matching nonempty root password. Password fields are masked; Tab switches fields.
3. Review the target and type the exact `ERASE /dev/vda` confirmation for that disk, then click **Erase & install**.
4. Wait for completion. Progress and errors appear in the wizard, while the desktop remains responsive. Closing the installer or exiting HOSWM is blocked during installation.

**Installation erases the entire selected disk.** Mounted disks, active swap, read-only devices and disks with device-mapper/RAID holders are excluded. The worker checks the target again and hashes the password before partitioning. Back or closing the window before installation leaves the disk untouched.

The wizard creates one bootable MBR partition, formats ext4 with label `HOSROOT`, copies the userspace and GUI SDK/demo, configures the hostname and root password, and installs BIOS GRUB. It uses `/boot/vmlinuz` from the live RAM filesystem, with optical-drive ISO search as a fallback. This works when the installer ISO was booted from a USB stick and is not exposed as a virtual CD/DVD drive. Disk discovery does not depend on how the system was booted or on a live-image marker. VirtualBox SATA disks appear as Linux devices such as `/dev/sda`. UEFI/GPT and partition-preserving installation are not supported. The old `hos-install` shell command has been removed.

After installation, power off and boot without the ISO:

```sh
tooling/qemu.sh disk
```

The installed boot entry loads the kernel directly with the ext4 root label. The native PID 1 mounts the installed system, starts the graphical login on `tty1`, and reaps orphaned processes. The installed system retains the userspace and utilities; the wizard can install onto another unused disk regardless of boot origin.

## Development checks

Existing tests can run without booting a VM:

```sh
CARGO_TARGET_DIR=.build/cargo cargo test --manifest-path HOSWM/Cargo.toml --locked --offline
python3 -m unittest discover -s tooling/tests -v
```

Rust tests cover drawing, framebuffer formats/scaling, keyboard modifiers, fonts, desktop controls, the menu bar, configuration parsing, input device handling, the QOI codec, the notification log, terminal behavior, and the protocol, including a compiled C client round trip. For the init system they cover the service protocol and its server loop, restart backoff and give-up, DHCP packets and leases, `wpa_supplicant` output, battery and interface sysfs parsing, SNTP replies, ALSA ioctl numbers and volume conversion, the software mixer's handshake and mixing, WAV decoding, and the settings application's pages. PTY and Unix-socket tests require a Linux environment that permits those operations; the C test requires `cc`. The Python test checks deterministic initramfs metadata and packaged files using fixtures. These checks do not replace a full build, VM boot or installation test.

## Troubleshooting

| Symptom | Checks |
| --- | --- |
| Missing host tool or wrong GRUB version | Install the named tool/version; set `HOS_GRUB_DIR` if modules are stored elsewhere |
| Archive checksum mismatch | Remove the named cached archive in the build downloads directory and rerun `fetch` |
| Incomplete extracted sources | Rerun `fetch`; extraction repairs incomplete trees and preserves the previous tree at the printed path |
| `sed: preserving permissions` / `chmod: Operation not permitted` for `modules.builtin.modinfo` | Rerun `tooling/build.sh kernel`; its metadata cleanup supports Windows mounts without Unix permissions and reuses existing objects |
| No framebuffer or black screen | Boot diagnostics; inspect `dmesg`, `/proc/fb`, and `ls /dev/fb*`; try the other graphical boot entry |
| Unsupported framebuffer format | Select a packed true-color 16-, 24- or 32-bit GRUB graphics mode |
| No keyboard/mouse | Plug one in; the session picks up devices while running and reports what it finds. Keyboards, relative mice and absolute tablets are all used. Check the serial log for `HOSWM input:` lines |
| Settings not applied | Restart the session; check the notifications naming the offending `~/.hoswm/config.ini` line |
| Graphics mode left active after forced termination | Reboot; normal exit and returned errors restore console mode |
| Installer cannot find an installation kernel | Boot a freshly rebuilt ISO and select **hOS RAM installer (USB / ISO)**; the live initramfs should contain `/boot/vmlinuz` |
| New driver configuration not reflected in ISO | Rebuild `kernel`, then `iso`, and reattach the generated image |
| A service is not answering | `hosctl init list` shows its state and failure count; the console carries its output. `hosctl init restart hos-netd` starts one that gave up |
| No network address | `hosctl net links` shows carrier and DHCP state; check that the interface has carrier, then `hosctl net renew` |
| Only `lo` is listed, or `Address family not supported by protocol` | The running kernel has no IPv4 stack or no driver for the adapter. Rebuild `kernel` and `iso` from the current `tooling/kernel.config`, and give the virtual machine a supported adapter, such as QEMU's virtio-net or VirtualBox's Intel PRO/1000 |
| Wi-Fi unavailable | The image was built with `HOS_WIFI=0`, so it carries no `wpa_supplicant`; rebuild the ISO without that setting. Wired networking is unaffected |
| Cannot join a WPA3 network | A supplicant built from source here has no TLS library and so no SAE; build with a distribution `wpa_supplicant` on the host or name one with `HOS_WPA_SUPPLICANT` |
| No sound | `hosctl sound status` names the default card and any device error; `hosctl sound play` a WAV file to test the mixer. A card with no mixer control reports `mixer=no` |
| Settings refused | Settings under `/etc/hos` need root; `power.conf` and `netd.conf` have `allow-users` for suspend, shutdown and joining networks |

## Current limits

hOS is an early system prototype. Boot media and installation currently use BIOS/MBR; EFI framebuffer support in the kernel does not provide a UEFI installer. There is no package manager or update workflow, and no general desktop application suite.

The system services are implemented but lightly exercised on real hardware. Wi-Fi drives the `wpa_supplicant` the image carries — the build host's, or one built from pinned upstream source and linked statically against musl. That source build carries wpa_supplicant's own crypto instead of a TLS library, so it joins WPA2 and open networks but not WPA3 (SAE); a host supplicant built against a TLS library, named with `HOS_WPA_SUPPLICANT`, covers those. Wired networking, DHCP and DNS do not depend on any of it. IPv6 is not configured. Sound covers playback: recording, per-application volume and hot-plugged cards changing the default device are not implemented, and cards without a mixer control fall back to software volume for what the service itself plays.

HOSWM uses a fixed logical 800×600 desktop and software rendering, with relative and absolute pointer input. It has a US keyboard map, a small bitmap font system, ANSI terminal emulation with scrollback, selection, multiline paste, and simple GUI controls. General window-edge resizing, full Unicode text editing, and application isolation are not implemented. The graphical login supports local accounts; the text diagnostic boot opens a root shell. The graphical terminal provides Bash completion, persistent shared history, Ctrl+R search, Page Up/Down scrollback with Shift, and safe multiline paste.

UEFI/GPT installation, a persistent package/update workflow, broader hardware testing, and richer desktop/application support remain future work. Bitmap text and the initial GUI API are already implemented.
