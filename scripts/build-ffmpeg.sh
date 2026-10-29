#!/bin/sh
# Builds the ffmpeg Lanes carries inside it (src/ytdlp.rs) and prints where it is, for build.rs through LANES_FFMPEG.
# It only joins a video to its sound as they are: every demuxer, muxer, parser and bitstream filter, the file
# protocol, and no outside library. The one codec is VP9's decoder: an MP4 has to state the pixel format of a VP9
# stream, and a WebM from YouTube (its 1440p and 2160p) does not say it, so ffmpeg reads the first frames for it.
# That keeps it about 4 MB, LGPL, and needing nothing installed. Built once per version and target into
# target/ffmpeg/; delete that folder after changing the options below.
#
#   scripts/build-ffmpeg.sh aarch64-apple-darwin
#   scripts/build-ffmpeg.sh x86_64-apple-darwin
#   scripts/build-ffmpeg.sh x86_64-pc-windows-msvc   # with the tools scripts/package-windows.sh checks for
set -eu
TARGET=${1:?usage: scripts/build-ffmpeg.sh <Rust target, such as aarch64-apple-darwin>}
VERSION=9.0.2
SHA256=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
cd "$(dirname "$0")/.."
ROOT=$(pwd)/target/ffmpeg
OUT=$ROOT/$VERSION/$TARGET
EXE=ffmpeg
case $TARGET in *windows*) EXE=ffmpeg.exe ;; esac
[ -f "$OUT/$EXE" ] && { echo "$OUT/$EXE"; exit 0; }

TARBALL=$ROOT/ffmpeg-$VERSION.tar.xz
if ! echo "$SHA256  $TARBALL" | shasum -a 256 -c >/dev/null 2>&1; then
    mkdir -p "$ROOT"
    curl -fL --retry 3 -o "$TARBALL.part" "https://ffmpeg.org/releases/ffmpeg-$VERSION.tar.xz" >&2
    echo "$SHA256  $TARBALL.part" | shasum -a 256 -c >/dev/null || { echo "ffmpeg-$VERSION.tar.xz is not the one expected" >&2; exit 1; }
    mv "$TARBALL.part" "$TARBALL"
fi
BUILD=$OUT/build
rm -rf "$BUILD"
mkdir -p "$BUILD"
tar -xJf "$TARBALL" -C "$BUILD" --strip-components 1

set -- --disable-everything --disable-autodetect --disable-asm --disable-doc --disable-debug --enable-small \
    --disable-ffplay --disable-ffprobe --disable-avdevice --disable-swscale --disable-network \
    --enable-protocol=file,pipe --enable-decoder=vp9 --enable-demuxers --enable-muxers --enable-parsers --enable-bsfs \
    --enable-cross-compile --arch="${TARGET%%-*}"
case $TARGET in
*-apple-darwin)
    # The macOS the app asks for in its Info.plist (scripts/bundle-macos.sh); clang calls aarch64 arm64.
    FLAGS="-arch $(echo "${TARGET%%-*}" | sed s/aarch64/arm64/) -mmacosx-version-min=13.0"
    set -- "$@" --target-os=darwin --extra-cflags="$FLAGS" --extra-ldflags="$FLAGS"
    ;;
*-windows-msvc)
    # The MSVC headers and libraries cargo-xwin keeps, found by clang-cl through CL_FLAGS and by lld-link through LIB.
    eval "$(cargo xwin env --target "$TARGET")"
    set -- "$@" --target-os=win64 --toolchain=msvc --cc=clang-cl --ld=lld-link --ar=llvm-ar --nm=llvm-nm \
        --host-cc=clang --extra-cflags="$CL_FLAGS"
    ;;
*)
    echo "no ffmpeg is built for $TARGET" >&2
    exit 1
    ;;
esac
(cd "$BUILD" && ./configure "$@" && make -j"$(sysctl -n hw.ncpu)") >"$OUT/build.log" 2>&1 ||
    { tail -30 "$OUT/build.log" >&2; echo "ffmpeg did not build; all of it is in $OUT/build.log" >&2; exit 1; }
cp "$BUILD/$EXE" "$OUT/$EXE"
rm -rf "$BUILD"
echo "$OUT/$EXE"
