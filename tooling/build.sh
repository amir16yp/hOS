#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/env.sh"
stage=${1:-all}
# Every Rust program packaged beside the window server: the desktop
# applications, the init system and its services.
HOS_APPS='hos-terminal hos-installer hos-about hos-files hos-image hos-paint hos-colorpicker hos-notifications hos-toast hos-account hos-settings hos-notepad hos-snake hos-init hos-netd hos-power hos-ntpd hos-soundd hosctl'
case "$stage" in fetch|kernel|userspace|hoswm|iso|all) ;; *) fail 'Usage: tooling/build.sh [fetch|kernel|userspace|hoswm|iso|all]' ;; esac
mkdir -p "$BUILD/downloads" "$BUILD/src" "$BUILD/out" "$BUILD/logs"
fetch_archive() {
    local name=$1 url=$2 hash=$3
    if [[ ! -f "$BUILD/downloads/$name" ]]; then
        need curl
        curl --fail --location --retry 3 "$url" -o "$BUILD/downloads/$name.part"
        mv "$BUILD/downloads/$name.part" "$BUILD/downloads/$name"
    fi
    echo "$hash  $BUILD/downloads/$name" | sha256sum --check || fail "Checksum mismatch: $name. Remove that archive and fetch again."
}
# Unpack a checksum-verified archive into $BUILD/src. The files named after the
# hash prove a tree is the complete source: a directory alone is not proof that
# a previous extraction finished.
extract_archive() {
    local name=$1 hash=$2
    shift 2
    local source_name=${name%.tar.*}
    local src="$BUILD/src/$source_name" complete=y proof
    [[ -f "$src/.hos-extracted-$hash" ]] || complete=
    for proof in "$@"; do [[ -e "$src/$proof" ]] || complete=; done
    # Publish only complete extractions, and repair caches made by older builds.
    if [[ -z "$complete" ]]; then
        local staging
        staging=$(mktemp -d "$BUILD/src/.extract-$source_name.XXXXXX")
        echo "Extracting $name"
        if ! tar -xf "$BUILD/downloads/$name" -C "$staging"; then
            fail "Extraction failed: $name (temporary files at $staging). Fix the tar error and rerun fetch."
        fi
        for proof in "$@"; do [[ -e "$staging/$source_name/$proof" ]] || fail "Incomplete source archive: $name"; done
        touch "$staging/$source_name/.hos-extracted-$hash"
        if [[ -e "$src" ]]; then
            # Preserve the old tree in case it contains local changes.
            mv "$src" "$staging/previous-source"
            echo "Previous source tree saved at $staging/previous-source"
        fi
        mv "$staging/$source_name" "$src"
        if [[ ! -e "$staging/previous-source" ]]; then rmdir "$staging"; fi
    fi
}
fetch_one() {
    fetch_archive "$@"
    extract_archive "$1" "$3" Makefile scripts/Kbuild.include
}
fetch() {
    if [[ "$HOS_KERNEL_MODE" == source ]]; then
        fetch_one "linux-$LINUX_VERSION.tar.xz" "https://cdn.kernel.org/pub/linux/kernel/v${LINUX_VERSION%%.*}.x/linux-$LINUX_VERSION.tar.xz" "$LINUX_SHA256"
    elif [[ "$HOS_KERNEL_MODE" != prebuilt ]]; then
        fail 'HOS_KERNEL_MODE must be source or prebuilt'
    fi
    fetch_coreutils
    # A host with no wpa_supplicant builds one, so its sources are downloads too.
    if [[ "${HOS_WIFI:-1}" != 0 ]]; then
        local supplicant
        supplicant=$(installed_wpa_supplicant)
        if [[ -z "$supplicant" ]]; then fetch_wifi_sources; fi
    fi
}
fetch_prebuilt_kernel() {
    local image_url=${HOS_PREBUILT_VMLINUZ_URL:-$PREBUILT_VMLINUZ_URL}
    local image_hash=${HOS_PREBUILT_VMLINUZ_SHA256:-$PREBUILT_VMLINUZ_SHA256}
    local config_url=${HOS_PREBUILT_KERNEL_CONFIG_URL:-$PREBUILT_KERNEL_CONFIG_URL}
    local config_hash=${HOS_PREBUILT_KERNEL_CONFIG_SHA256:-$PREBUILT_KERNEL_CONFIG_SHA256}
    [[ -n "$image_url" && -n "$image_hash" && -n "$config_url" && -n "$config_hash" ]] || fail 'Prebuilt mode needs HOS_PREBUILT_VMLINUZ_URL/SHA256 and HOS_PREBUILT_KERNEL_CONFIG_URL/SHA256; URLs must be pinned direct downloads.'
    need curl
    for item in "vmlinuz|$image_url|$image_hash" "kernel.config|$config_url|$config_hash"; do
        IFS='|' read -r name url hash <<< "$item"
        local dest="$BUILD/downloads/prebuilt-$name"
        if [[ ! -f "$dest" ]]; then curl --fail --location --retry 3 "$url" -o "$dest.part"; mv "$dest.part" "$dest"; fi
        echo "$hash  $dest" | sha256sum --check || fail "Prebuilt $name checksum mismatch; remove $dest and retry."
    done
    python3 "$ROOT/tooling/verify-prebuilt-kernel.py" "$BUILD/downloads/prebuilt-vmlinuz" "$BUILD/downloads/prebuilt-kernel.config" || fail 'Configured prebuilt kernel is incompatible. Complete tooling/build.sh kernel or supply a compatible HOS_PREBUILT_VMLINUZ_URL/SHA256 and HOS_PREBUILT_KERNEL_CONFIG_URL/SHA256 pair.'
    cp "$BUILD/downloads/prebuilt-vmlinuz" "$BUILD/out/vmlinuz"
    cp "$BUILD/downloads/prebuilt-kernel.config" "$BUILD/out/kernel.config"
}
kernel() {
    if [[ "$HOS_KERNEL_MODE" == prebuilt ]]; then fetch_prebuilt_kernel; return; fi
    [[ "$HOS_KERNEL_MODE" == source ]] || fail 'HOS_KERNEL_MODE must be source or prebuilt'
    for tool in make gcc flex bison bc python3; do need "$tool"; done
    local src="$BUILD/src/linux-$LINUX_VERSION" obj="$BUILD/kernel"
    [[ -d "$src" ]] || fail 'Run tooling/build.sh fetch first'
    mkdir -p "$obj"
    export KBUILD_BUILD_TIMESTAMP="$(date -u -d "@$SOURCE_DATE_EPOCH" '+%a %b %d %T UTC %Y')" KBUILD_BUILD_USER=hos KBUILD_BUILD_HOST=builder KBUILD_BUILD_VERSION=1
    make -C "$src" O="$obj" ARCH=x86_64 KCONFIG_ALLCONFIG="$ROOT/tooling/kernel.config" allnoconfig
    # Missing dependencies must not silently remove the devices needed for boot.
    for symbol in 64BIT BLK_DEV_INITRD BLK_DEV DEVTMPFS DEVTMPFS_MOUNT PROC_FS SYSFS TTY UNIX98_PTYS UNIX NET INET PACKET NETDEVICES ETHERNET VIRTIO_NET E1000 E1000E R8169 PCNET32 CFG80211 MAC80211 WLAN SERIAL_8250 SERIAL_8250_CONSOLE PCI VIRTIO_MENU VIRTIO_PCI VIRTIO_BLK SCSI SCSI_LOWLEVEL SCSI_VIRTIO ISO9660_FS BLK_DEV_SR BLK_DEV_SD DRM DRM_VIRTIO_GPU DRM_VIRTIO_GPU_KMS INPUT INPUT_EVDEV INPUT_KEYBOARD KEYBOARD_ATKBD INPUT_MOUSE MOUSE_PS2 SERIO_I8042 VT VT_CONSOLE FB FB_DEVICE FB_VESA EFI FB_EFI FRAMEBUFFER_CONSOLE VGA_CONSOLE DRM_FBDEV_EMULATION DRM_BOCHS DRM_VMWGFX DRM_VBOXVIDEO ATA_PIIX SATA_AHCI USB USB_STORAGE USB_UAS USB_HID USB_XHCI_PCI USB_EHCI_PCI USB_OHCI_HCD_PCI USB_UHCI_HCD EXT2_FS EXT4_FS; do
        grep -qx "CONFIG_$symbol=y" "$obj/.config" || fail "Kernel CONFIG_$symbol was not enabled; inspect $obj/.config"
    done
    # Kbuild's sed -i/chmod step fails on Windows mounts without Unix metadata.
    # Override only this recipe; keep the source cache and existing objects intact.
    export HOS_MODINFO_HELPER="$ROOT/tooling/fix-kernel-modinfo.py"
    make -C "$src" O="$obj" ARCH=x86_64 -j"$JOBS" \
        'cmd_modules_builtin_modinfo=$(cmd_objcopy) && python3 "$$HOS_MODINFO_HELPER" "$@"' bzImage
    python3 "$ROOT/tooling/verify-prebuilt-kernel.py" "$obj/arch/x86/boot/bzImage" "$obj/.config"
    cp "$obj/arch/x86/boot/bzImage" "$BUILD/out/vmlinuz"
    cp "$obj/.config" "$BUILD/out/kernel.config"
}
fetch_coreutils() {
    local name="coreutils-$UUTILS_VERSION-x86_64-unknown-linux-musl.tar.gz"
    need curl
    if [[ ! -f "$BUILD/downloads/$name" ]]; then
        curl --fail --location --retry 3 "https://github.com/uutils/coreutils/releases/download/$UUTILS_VERSION/$name" -o "$BUILD/downloads/$name.part"
        mv "$BUILD/downloads/$name.part" "$BUILD/downloads/$name"
    fi
    echo "$UUTILS_SHA256  $BUILD/downloads/$name" | sha256sum --check || fail "uutils checksum mismatch: $name"
}
userspace() {
    for tool in gcc readelf python3; do need "$tool"; done
    fetch_coreutils
    local name="coreutils-$UUTILS_VERSION-x86_64-unknown-linux-musl"
    tar -xmzf "$BUILD/downloads/$name.tar.gz" -C "$BUILD/src"
    cp "$BUILD/src/$name/coreutils" "$BUILD/out/coreutils"
    chmod +x "$BUILD/out/coreutils"
    if readelf -l "$BUILD/out/coreutils" | grep -q INTERP; then fail 'coreutils must be statically linked'; fi
    "$BUILD/out/coreutils" --list > "$BUILD/out/coreutils-applets"
    cp "$BUILD/src/$name/LICENSE" "$BUILD/out/coreutils-LICENSE"
    gcc -std=c11 -O2 -Wall -Wextra -Werror -static "$ROOT/tooling/hos-password.c" -lcrypt -o "$BUILD/out/hos-password"
}
hoswm() {
    for tool in cargo gcc ar readelf; do need "$tool"; done
    CARGO_TARGET_DIR="$BUILD/cargo" RUSTFLAGS='-C target-feature=+crt-static' cargo build --manifest-path "$ROOT/HOSWM/Cargo.toml" --release --locked --offline
    cp "$BUILD/cargo/release/hoswm" "$BUILD/out/hoswm"
    for app in $HOS_APPS; do cp "$BUILD/cargo/release/$app" "$BUILD/out/$app"; done
    for app in hoswm $HOS_APPS; do if readelf -l "$BUILD/out/$app" | grep -q INTERP; then fail "$app must be statically linked"; fi; done
    gcc -std=c11 -O2 -Wall -Wextra -Werror -I "$ROOT/HOSWM/include" -c "$ROOT/HOSWM/src/client.c" -o "$BUILD/out/hoswm-client.o"
    ar rcsD "$BUILD/out/libhoswm.a" "$BUILD/out/hoswm-client.o"
    cp "$ROOT/HOSWM/include/hoswm.h" "$BUILD/out/hoswm.h"
    gcc -std=c11 -O2 -Wall -Wextra -Werror -static -I "$ROOT/HOSWM/include" "$ROOT/HOSWM/examples/hello_gui.c" "$BUILD/out/libhoswm.a" -o "$BUILD/out/hos-hello"
}
# Wi-Fi: hos-netd drives wpa_supplicant over its control socket, so the image
# carries one. It lives in sbin, which is not always on a user's PATH. Prints
# nothing when the host has none; the build then makes one from source.
installed_wpa_supplicant() {
    local candidate directory
    if [[ -n "${HOS_WPA_SUPPLICANT:-}" ]]; then
        [[ -x "$HOS_WPA_SUPPLICANT" ]] || fail "HOS_WPA_SUPPLICANT is not an executable: $HOS_WPA_SUPPLICANT"
        echo "$HOS_WPA_SUPPLICANT"
        return
    fi
    candidate=$(command -v wpa_supplicant || true)
    if [[ -z "$candidate" ]]; then
        for directory in "${HOST:-/usr}/sbin" /usr/local/sbin /usr/sbin /sbin; do
            if [[ -x "$directory/wpa_supplicant" ]]; then candidate="$directory/wpa_supplicant"; break; fi
        done
    fi
    echo "$candidate"
}
fetch_wifi_sources() {
    fetch_archive "musl-$MUSL_VERSION.tar.gz" "https://musl.libc.org/releases/musl-$MUSL_VERSION.tar.gz" "$MUSL_SHA256"
    fetch_archive "libnl-$LIBNL_VERSION.tar.gz" "https://github.com/thom311/libnl/releases/download/libnl${LIBNL_VERSION//./_}/libnl-$LIBNL_VERSION.tar.gz" "$LIBNL_SHA256"
    fetch_archive "wpa_supplicant-$WPA_SUPPLICANT_VERSION.tar.gz" "https://w1.fi/releases/wpa_supplicant-$WPA_SUPPLICANT_VERSION.tar.gz" "$WPA_SUPPLICANT_SHA256"
}
# musl-gcc drops the host include path, but the Linux userspace headers that
# the netlink library and the nl80211 driver include live only there.
# -idirafter searches them after musl's own headers, which keeps musl's
# definitions of the C library ahead of the host's.
musl_cflags() {
    [[ -d /usr/include/linux ]] || fail 'Missing Linux userspace headers (/usr/include/linux) for the wpa_supplicant build. Install them (linux-libc-dev), install wpa_supplicant, or point HOS_WPA_SUPPLICANT at a binary.'
    local flags='-O2 -static -idirafter /usr/include' multiarch
    multiarch=$(gcc -print-multiarch 2>/dev/null || true)
    if [[ -n "$multiarch" && -d "/usr/include/$multiarch" ]]; then flags="$flags -idirafter /usr/include/$multiarch"; fi
    echo "$flags"
}
# The C library the supplicant links against. Installing it here leaves the
# host's own compiler and libraries untouched.
build_musl() {
    local prefix="$BUILD/wifi/musl-$MUSL_VERSION" obj="$BUILD/wifi/musl-$MUSL_VERSION-obj"
    if [[ -x "$prefix/bin/musl-gcc" ]]; then return; fi
    fetch_archive "musl-$MUSL_VERSION.tar.gz" "https://musl.libc.org/releases/musl-$MUSL_VERSION.tar.gz" "$MUSL_SHA256"
    extract_archive "musl-$MUSL_VERSION.tar.gz" "$MUSL_SHA256" Makefile configure
    mkdir -p "$obj"
    (cd "$obj" && "$BUILD/src/musl-$MUSL_VERSION/configure" --prefix="$prefix" --disable-shared)
    make -C "$obj" -j"$JOBS" install
    [[ -x "$prefix/bin/musl-gcc" ]] || fail 'The musl build produced no musl-gcc'
}
# The netlink library behind wpa_supplicant's nl80211 driver, built against
# that same musl so the supplicant can link it statically.
build_libnl() {
    local prefix="$BUILD/wifi/libnl-$LIBNL_VERSION" obj="$BUILD/wifi/libnl-$LIBNL_VERSION-obj" cflags pkgconfig
    if [[ -f "$prefix/lib/libnl-genl-3.a" ]]; then return; fi
    fetch_archive "libnl-$LIBNL_VERSION.tar.gz" "https://github.com/thom311/libnl/releases/download/libnl${LIBNL_VERSION//./_}/libnl-$LIBNL_VERSION.tar.gz" "$LIBNL_SHA256"
    extract_archive "libnl-$LIBNL_VERSION.tar.gz" "$LIBNL_SHA256" configure include/netlink/netlink.h
    cflags=$(musl_cflags)
    # libnl's configure insists on pkg-config, but only to look for the unit
    # test framework. Naming that framework's flags here leaves the tests out,
    # so a host without pkg-config still builds the library.
    pkgconfig=$(command -v pkg-config || type -P true)
    mkdir -p "$obj"
    (cd "$obj" && CC="$BUILD/wifi/musl-$MUSL_VERSION/bin/musl-gcc" CFLAGS="$cflags" LDFLAGS=-static         PKG_CONFIG="$pkgconfig" CHECK_CFLAGS=' ' CHECK_LIBS=' '         "$BUILD/src/libnl-$LIBNL_VERSION/configure" --prefix="$prefix" --disable-shared --enable-static --disable-cli --disable-debug)
    make -C "$obj" -j"$JOBS"
    make -C "$obj" install
    [[ -f "$prefix/lib/libnl-genl-3.a" ]] || fail 'The libnl build produced no static netlink library'
}
# Build the supplicant itself from the pinned upstream release.
build_wpa_supplicant() {
    for tool in make gcc flex bison readelf strip; do need "$tool"; done
    build_musl
    build_libnl
    local musl="$BUILD/wifi/musl-$MUSL_VERSION" libnl="$BUILD/wifi/libnl-$LIBNL_VERSION" cflags
    cflags=$(musl_cflags)
    fetch_archive "wpa_supplicant-$WPA_SUPPLICANT_VERSION.tar.gz" "https://w1.fi/releases/wpa_supplicant-$WPA_SUPPLICANT_VERSION.tar.gz" "$WPA_SUPPLICANT_SHA256"
    extract_archive "wpa_supplicant-$WPA_SUPPLICANT_VERSION.tar.gz" "$WPA_SUPPLICANT_SHA256" wpa_supplicant/Makefile src/drivers/driver_nl80211.c
    local src="$BUILD/src/wpa_supplicant-$WPA_SUPPLICANT_VERSION/wpa_supplicant"
    # wpa_supplicant reads .config as make syntax. It selects what hos-netd
    # drives -- the nl80211 driver, the control socket and a configuration
    # file the supplicant may rewrite -- and, with no pkg-config in the
    # picture, names the netlink library and the static musl link directly.
    # wpa_supplicant's own crypto keeps a TLS library out of the image; WPA3
    # (SAE) needs one, so this build connects to WPA2 and open networks.
    cat > "$src/.config.new" <<EOF
CONFIG_DRIVER_NL80211=y
CONFIG_LIBNL32=y
LIBNL_INC=$libnl/include/libnl3
CONFIG_CTRL_IFACE=y
CONFIG_BACKEND=file
CONFIG_TLS=internal
CONFIG_INTERNAL_LIBTOMMATH=y
CFLAGS += $cflags
LIBS += -L$libnl/lib -static
EOF
    # An unchanged configuration keeps its timestamp so make reuses its objects.
    if cmp -s "$src/.config.new" "$src/.config"; then rm "$src/.config.new"; else mv "$src/.config.new" "$src/.config"; fi
    make -C "$src" -j"$JOBS" CC="$musl/bin/musl-gcc" wpa_supplicant
    cp "$src/wpa_supplicant" "$BUILD/out/wpa_supplicant"
    # Debugging symbols are three quarters of a binary the image holds in RAM.
    strip "$BUILD/out/wpa_supplicant"
    if readelf -l "$BUILD/out/wpa_supplicant" | grep -q INTERP; then fail 'The built wpa_supplicant must be statically linked'; fi
}
find_wpa_supplicant() {
    local candidate
    candidate=$(installed_wpa_supplicant)
    if [[ -z "$candidate" ]]; then
        # Build output belongs on the terminal, not in this function's result.
        echo 'No wpa_supplicant on this host; building one from source.' >&2
        build_wpa_supplicant >&2
        candidate="$BUILD/out/wpa_supplicant"
    fi
    echo "$candidate"
}
select_iso_kernel() {
    # Only use the published pair. Interrupted builds can leave a new .config
    # beside an older or incomplete bzImage in the kernel build directory.
    if [[ -f "$BUILD/out/vmlinuz" && -f "$BUILD/out/kernel.config" ]]; then
        if python3 "$ROOT/tooling/verify-prebuilt-kernel.py" "$BUILD/out/vmlinuz" "$BUILD/out/kernel.config"; then
            echo "Using existing kernel: $BUILD/out/vmlinuz"
            return
        fi
        echo 'Existing kernel is incompatible; trying the configured prebuilt kernel.' >&2
    else
        echo 'No complete kernel image/config pair; trying the configured prebuilt kernel.' >&2
    fi
    fetch_prebuilt_kernel
}
iso() {
    # Always package the current window server and C ABI, including the GUI demo.
    userspace
    hoswm
    for tool in python3 grub-mkrescue xorriso readelf mkfs.ext4; do need "$tool"; done
    [[ -f "$GRUB_DIR/boot_hybrid.img" ]] || fail "Missing GRUB BIOS modules at $GRUB_DIR; install grub-pc-bin or set HOS_GRUB_DIR"
    for tool in grub-install grub-mkimage grub-probe; do need "$tool"; done
    [[ "$(grub-install --version)" == *"$GRUB_VERSION"* ]] || fail "Expected GRUB $GRUB_VERSION; found $(grub-install --version)"
    select_iso_kernel
    if [[ "${HOS_WIFI:-1}" == 0 ]]; then
        echo 'HOS_WIFI=0: building without wpa_supplicant; Wi-Fi will report that it is unavailable.' >&2
        export HOS_WPA_SUPPLICANT=
    else
        HOS_WPA_SUPPLICANT=$(find_wpa_supplicant)
        export HOS_WPA_SUPPLICANT
        echo "Bundling Wi-Fi supplicant: $HOS_WPA_SUPPLICANT"
    fi
    for file in vmlinuz coreutils hos-password hoswm $HOS_APPS; do [[ -f "$BUILD/out/$file" ]] || fail "Missing $file; run tooling/build.sh all"; done
    python3 "$ROOT/tooling/mkrootfs.py" "$BUILD" "$GRUB_DIR" "$(command -v grub-install)" "$(command -v grub-mkimage)" "$(command -v grub-probe)"
    mkdir -p "$BUILD/iso/boot/grub"
    cp "$BUILD/out/vmlinuz" "$BUILD/out/initramfs.cpio.gz" "$BUILD/iso/boot/"
    cp "$ROOT/tooling/grub.cfg" "$BUILD/iso/boot/grub/grub.cfg"
    echo hOS-0.1 > "$BUILD/iso/HOS_RELEASE"
    grub-mkrescue -d "$GRUB_DIR" -o "$BUILD/out/hOS.iso" "$BUILD/iso" -- -volid hOS -volume_date all="$(date -u -d "@$SOURCE_DATE_EPOCH" +%Y%m%d%H%M%S)00"
    {
        cat "$ROOT/tooling/versions.env"
        rustc --version; cargo --version; gcc --version | head -1
        grub-install --version; grub-mkrescue --version; xorriso -version 2>&1 | head -1
        apps=(); for app in $HOS_APPS; do apps+=("$BUILD/out/$app"); done
        # The supplicant may come from the host or from this build's sources,
        # so the manifest records the binary that the image actually carries.
        if [[ -n "${HOS_WPA_SUPPLICANT:-}" ]]; then apps+=("$HOS_WPA_SUPPLICANT"); fi
        sha256sum "$BUILD/out/libhoswm.a" "$BUILD/out/hoswm.h" "$BUILD/out/hos-hello" "$BUILD/out/hoswm" "${apps[@]}" "$BUILD/out/coreutils" "$BUILD/out/coreutils-applets" "$BUILD/out/hos-password" "$BUILD/out/vmlinuz" "$BUILD/out/initramfs.cpio.gz" "$BUILD/out/hOS.iso"
    } > "$BUILD/out/build-manifest.txt"
    echo "Built $BUILD/out/hOS.iso"
}
if [[ "$stage" == all ]]; then fetch; kernel; iso; else "$stage"; fi
