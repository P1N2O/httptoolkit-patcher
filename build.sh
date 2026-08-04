#!/bin/sh
# Build release binaries for every supported OS/arch into dist/.
#
# Native build (this machine's OS/arch):
#   ./build.sh
#
# Cross builds (all targets) need the zig-based linker:
#   rustup target add aarch64-unknown-linux-gnu x86_64-pc-windows-gnu \
#     x86_64-apple-darwin aarch64-apple-darwin
#   cargo install cargo-zigbuild
#   ./build.sh all
#
# (Windows ARM64 is not covered: rustup ships no aarch64-pc-windows-gnu std.)
set -e

TARGETS="x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu x86_64-pc-windows-gnu x86_64-apple-darwin aarch64-apple-darwin"
mkdir -p dist

name_for() {
    case "$1" in
        x86_64-unknown-linux-gnu) echo "linux-x86_64" ;;
        aarch64-unknown-linux-gnu) echo "linux-aarch64" ;;
        x86_64-pc-windows-gnu) echo "windows-x86_64" ;;
        x86_64-apple-darwin) echo "mac-x86_64" ;;
        aarch64-apple-darwin) echo "mac-aarch64" ;;
        *) echo "unknown" ;;
    esac
}

build_one() {
    t="$1"
    echo "==> $t"
    if command -v cargo-zigbuild >/dev/null 2>&1; then
        cargo zigbuild --release --target "$t"
    else
        cargo build --release --target "$t"
    fi
    case "$t" in
        *windows*) ext=".exe" ;;
        *) ext="" ;;
    esac
    cp "target/$t/release/httptoolkit-patcher$ext" "dist/httptoolkit-patcher-$(name_for "$t")$ext"
}

if [ "$1" = "all" ]; then
    for t in $TARGETS; do build_one "$t"; done
else
    build_one "$(rustc -vV | sed -n 's/^host: //p')"
fi

echo "==> dist/"
ls -l dist/
