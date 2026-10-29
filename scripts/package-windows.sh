#!/bin/sh
# Builds the Windows app here and zips it as dist/Lanes-<version>-windows-x64.zip with Lanes.exe in it. It is
# cross-compiled for MSVC with cargo-xwin, which fetches the MSVC CRT and the Windows SDK once into its cache.
# Nothing of it runs here, so the Windows tests do not.
#
#   scripts/package-windows.sh
#   CHECK=1 scripts/package-windows.sh   # only checks the tools, so a release stops before it builds anything
set -eu
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ZIP="Lanes-$VERSION-windows-x64.zip"

LLVM=$(brew --prefix llvm 2>/dev/null || :)/bin
LLD=$(brew --prefix lld 2>/dev/null || :)/bin
[ -x "$LLVM/clang-cl" ] && [ -x "$LLVM/llvm-lib" ] && [ -x "$LLVM/llvm-rc" ] && [ -x "$LLD/lld-link" ] ||
    { echo "the Windows app needs Homebrew's LLVM; install it once: brew install llvm lld" >&2; exit 1; }
command -v cargo-xwin >/dev/null ||
    { echo "the Windows app needs cargo-xwin; install it once: cargo install --locked cargo-xwin" >&2; exit 1; }
rustup target list --installed | grep -qx x86_64-pc-windows-msvc ||
    { echo "the Windows app needs Rust for Windows; add it once: rustup target add x86_64-pc-windows-msvc" >&2; exit 1; }
[ "${CHECK:-}" = 1 ] && exit 0
PATH="$LLVM:$LLD:$PATH"

# crt-static, because the bundled C of sqlite links the static C runtime; /ignore:4099 quiets the linker warnings
# that Microsoft's CRT ships without PDB files.
RUSTFLAGS="-C target-feature=+crt-static -C link-arg=/ignore:4099" \
    cargo xwin build --release --locked --target x86_64-pc-windows-msvc
OUT=${CARGO_TARGET_DIR:-target}/x86_64-pc-windows-msvc/release
# Staged in a folder of its own, so the capitalised name is kept even on a case-insensitive Mac disk where
# Lanes.exe and lanes.exe would otherwise be the same file.
STAGE=$(mktemp -d)
cp "$OUT/lanes.exe" "$STAGE/Lanes.exe"
mkdir -p dist
rm -f "dist/$ZIP"
zip -qj "dist/$ZIP" "$STAGE/Lanes.exe"
rm -rf "$STAGE"
echo "dist/$ZIP"
