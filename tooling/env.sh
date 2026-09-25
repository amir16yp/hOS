#!/usr/bin/env bash
# Source from host scripts. HOS_BUILD_DIR may point to a Linux filesystem for speed.
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
BUILD=${HOS_BUILD_DIR:-$ROOT/.build}
mkdir -p "$BUILD"
BUILD=$(cd "$BUILD" && pwd)
source "$ROOT/tooling/versions.env"
HOS_KERNEL_MODE=${HOS_KERNEL_MODE:-source}
export HOS_KERNEL_MODE
export SOURCE_DATE_EPOCH LC_ALL=C TZ=UTC
# Optional locally extracted host packages (used by the development environment).
if [[ -d "$ROOT/.build/host/usr/bin" ]]; then
    HOST="$ROOT/.build/host/usr"
    export PATH="$HOST/bin:$PATH"
    export LD_LIBRARY_PATH="$HOST/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    export BISON_PKGDATADIR="$HOST/share/bison"
    # The relocatable Bison package from the local host cache was built with
    # /usr/bin/m4 as its default, which may not exist on the build host.
    [[ -x "$HOST/bin/m4" ]] && export M4="$HOST/bin/m4"
    export HOSTCFLAGS="${HOSTCFLAGS:-} -I$HOST/include"
    export HOSTLDFLAGS="${HOSTLDFLAGS:-} -L$HOST/lib/x86_64-linux-gnu -l:libz.so.1 -l:libzstd.so.1"
fi
GRUB_DIR=${HOS_GRUB_DIR:-${HOST:-/usr}/lib/grub/i386-pc}
JOBS=${JOBS:-$(nproc)}
fail() { echo "ERROR: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null || fail "Missing $1. See README host dependencies."; }
