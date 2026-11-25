#!/usr/bin/env bash
# Build ccmon GUI without sudo: WebKitGTK/GTK headers come from ~/.local/sysroot
# (extracted -dev .debs); runtime libraries are the system ones.
set -euo pipefail
cd "$(dirname "$0")"
SYSROOT="${CCMON_SYSROOT:-$HOME/.local/sysroot}"
if [ -d "$SYSROOT" ] && ! pkg-config --exists webkit2gtk-4.1 2>/dev/null; then
    export PKG_CONFIG_SYSROOT_DIR="$SYSROOT"
    export PKG_CONFIG_LIBDIR="$SYSROOT/usr/lib/x86_64-linux-gnu/pkgconfig:$SYSROOT/usr/share/pkgconfig"
fi
export PATH="$HOME/.cargo/bin:$PATH"
(cd ui && yarn install --silent && yarn -s build)
cargo build --release -p ccmon-gui
echo "Binary: $(cd .. && pwd)/target/release/ccmon-gui"
