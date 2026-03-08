#!/usr/bin/env bash
# scripts/build_ffmpeg.sh — Cross-compile dav1d + FFmpeg for the specified target.
#
# Usage:
#   ./scripts/build_ffmpeg.sh --target <rust-target-triple> --install-dir <path>
#
# SPDX-License-Identifier: LGPL-2.1-or-later

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FFMPEG_SRC="$REPO_ROOT/ffmpeg"
DAV1D_SRC="$REPO_ROOT/dav1d"

# ── Argument parsing ──────────────────────────────────────────────────────────

TARGET=""
INSTALL_DIR=""
FFMPEG_SRC_OVERRIDE=""
DAV1D_SRC_OVERRIDE=""
DAV1D_INSTALL_OVERRIDE=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target)            TARGET="$2";               shift 2 ;;
        --install-dir)       INSTALL_DIR="$2";           shift 2 ;;
        --ffmpeg-src)        FFMPEG_SRC_OVERRIDE="$2";   shift 2 ;;
        --dav1d-src)         DAV1D_SRC_OVERRIDE="$2";    shift 2 ;;
        --dav1d-install-dir) DAV1D_INSTALL_OVERRIDE="$2"; shift 2 ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
done

if [[ -z "$TARGET" || -z "$INSTALL_DIR" ]]; then
    echo "Usage: $0 --target <rust-target-triple> --install-dir <path> [--ffmpeg-src <path>] [--dav1d-src <path>] [--dav1d-install-dir <path>]" >&2
    exit 1
fi

# Allow overriding source directories (e.g. when called from build.rs).
FFMPEG_SRC="${FFMPEG_SRC_OVERRIDE:-$REPO_ROOT/ffmpeg}"
DAV1D_SRC="${DAV1D_SRC_OVERRIDE:-$REPO_ROOT/dav1d}"

BUILD_DIR="$INSTALL_DIR/../ffmpeg_build_$TARGET"
DAV1D_BUILD_DIR="$INSTALL_DIR/../dav1d_build_$TARGET"
DAV1D_INSTALL_DIR="${DAV1D_INSTALL_OVERRIDE:-$INSTALL_DIR}"
mkdir -p "$BUILD_DIR" "$INSTALL_DIR" "$DAV1D_BUILD_DIR" "$DAV1D_INSTALL_DIR"

# ── Target mapping ────────────────────────────────────────────────────────────

# Derive FFmpeg arch/target-os and cross-compile toolchain prefix from the Rust
# target triple.
EXTRA_LDFLAGS=""
case "$TARGET" in
    x86_64-unknown-linux-gnu)
        FF_ARCH=x86_64; FF_OS=linux; CROSS_PREFIX=""; EXTRA_CFLAGS="-fPIC"
        CLANG_TRIPLE=""; CLANG_GCC_TOOLCHAIN=""; DAV1D_CROSS_FILE="" ;;
    aarch64-unknown-linux-gnu)
        FF_ARCH=aarch64; FF_OS=linux; CROSS_PREFIX="aarch64-linux-gnu-"; EXTRA_CFLAGS="-fPIC"
        CLANG_TRIPLE="aarch64-linux-gnu"
        # On Debian/Ubuntu multiarch, --print-sysroot returns '/' which is useless.
        # --gcc-toolchain tells clang to locate the cross GCC and its multiarch paths.
        CLANG_GCC_TOOLCHAIN="/usr"
        DAV1D_CROSS_FILE="$DAV1D_SRC/package/crossfiles/aarch64-linux-clang.meson" ;;
    x86_64-pc-windows-gnu)
        FF_ARCH=x86_64; FF_OS=mingw32; CROSS_PREFIX="x86_64-w64-mingw32-"; EXTRA_CFLAGS=""
        CLANG_TRIPLE="x86_64-w64-mingw32"; CLANG_GCC_TOOLCHAIN="/usr"
        DAV1D_CROSS_FILE="$DAV1D_SRC/package/crossfiles/x86_64-w64-mingw32.meson"  ;;
    x86_64-apple-darwin)
        FF_ARCH=x86_64; FF_OS=darwin; CROSS_PREFIX=""; EXTRA_CFLAGS=""
        CLANG_TRIPLE=""; CLANG_GCC_TOOLCHAIN=""; DAV1D_CROSS_FILE="" ;;
    aarch64-apple-darwin)
        FF_ARCH=aarch64; FF_OS=darwin; CROSS_PREFIX=""; EXTRA_CFLAGS=""
        CLANG_TRIPLE=""; CLANG_GCC_TOOLCHAIN=""; DAV1D_CROSS_FILE="" ;;
    aarch64-linux-android)
        FF_ARCH=aarch64; FF_OS=android; CROSS_PREFIX="${ANDROID_CROSS_PREFIX:-llvm-}"; EXTRA_CFLAGS="-fPIC"
        CLANG_TRIPLE=""; CLANG_GCC_TOOLCHAIN=""; DAV1D_CROSS_FILE="$DAV1D_SRC/package/crossfiles/aarch64-android.meson" ;;
    aarch64-apple-ios)
        FF_ARCH=aarch64; FF_OS=darwin; CROSS_PREFIX=""; EXTRA_CFLAGS="-arch arm64 -mios-version-min=13.0 -isysroot $(xcrun --sdk iphoneos --show-sdk-path 2>/dev/null || echo '')"
        EXTRA_LDFLAGS="$EXTRA_CFLAGS"
        CLANG_TRIPLE=""; CLANG_GCC_TOOLCHAIN=""; DAV1D_CROSS_FILE="$DAV1D_SRC/package/crossfiles/arm64-iPhoneOS.meson" ;;
    *)
        echo "Unsupported target triple: $TARGET" >&2
        exit 1 ;;
esac

HOST_TARGET="$(uname -m)-$(uname -s | tr '[:upper:]' '[:lower:]')"
JOBS="${MAKE_JOBS:-$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)}"

if [[ "$FF_OS" == "android" ]]; then
    PATH="${ANDROID_NDK_HOME}/toolchains/llvm/prebuilt/linux-x86_64/bin:$PATH"
fi

CC="${CC:-clang}"

# ── Build dav1d ───────────────────────────────────────────────────────────────

echo "==> Building dav1d for target: $TARGET"

DAV1D_MESON_ARGS=(
    "--prefix=$DAV1D_INSTALL_DIR"
    "--libdir=lib"
    "--default-library=static"
    "--buildtype=release"
    "-Denable_tools=false"
    "-Denable_examples=false"
    "-Denable_tests=false"
    "-Denable_docs=false"
)

# Pass the pre-built cross file when cross-compiling.
if [[ -n "${DAV1D_CROSS_FILE:-}" ]]; then
    DAV1D_MESON_ARGS+=("--cross-file=$DAV1D_CROSS_FILE")
fi
# Tell meson/clang where the GCC cross-toolchain is so library probes succeed.
# NOTE: Passing -Dc_args overrides the cross file c_args, so we must also specify --target.
if [[ -n "${CLANG_GCC_TOOLCHAIN:-}" && "$TARGET" != *"windows"* ]]; then
    DAV1D_MESON_ARGS+=("-Dc_args=--target=$CLANG_TRIPLE --gcc-toolchain=$CLANG_GCC_TOOLCHAIN" "-Dc_link_args=--target=$CLANG_TRIPLE --gcc-toolchain=$CLANG_GCC_TOOLCHAIN")
fi

meson setup --reconfigure "$DAV1D_BUILD_DIR" "$DAV1D_SRC" "${DAV1D_MESON_ARGS[@]}"
ninja -C "$DAV1D_BUILD_DIR"
meson install -C "$DAV1D_BUILD_DIR"

# ── Configure FFmpeg ──────────────────────────────────────────────────────────

CONFIGURE_ARGS=(
    "$FFMPEG_SRC/configure"
    "--prefix=$INSTALL_DIR"
    "--disable-everything"
    "--disable-programs"
    "--disable-doc"
    "--disable-htmlpages"
    "--disable-manpages"
    "--disable-podpages"
    "--disable-txtpages"
    "--disable-network"
    "--disable-autodetect"
    "--disable-iconv"
    "--disable-sdl2"
    "--enable-small"
    "--enable-decoder=vp9"
    "--enable-decoder=av1"
    "--enable-libdav1d"
    "--enable-decoder=libdav1d"
    "--enable-swscale"
    "--enable-static"
    "--disable-shared"
    "--enable-pic"
    "--arch=$FF_ARCH"
    "--target-os=$FF_OS"
    "--extra-cflags=-I${DAV1D_INSTALL_DIR}/include${EXTRA_CFLAGS:+ $EXTRA_CFLAGS}"
    "--extra-ldflags=-L${DAV1D_INSTALL_DIR}/lib${EXTRA_LDFLAGS:+ $EXTRA_LDFLAGS}"
)

# Threading model.
if [[ "$FF_OS" == "mingw32" ]]; then
    CONFIGURE_ARGS+=("--enable-w32threads")
else
    CONFIGURE_ARGS+=("--enable-pthreads")
fi

# Cross-compilation settings.
if [[ "$TARGET" != *"$(uname -m)"* ]] || [[ "$CROSS_PREFIX" != "" ]]; then
    CONFIGURE_ARGS+=("--enable-cross-compile")
fi

if [[ -n "$CROSS_PREFIX" ]]; then
    CONFIGURE_ARGS+=("--cross-prefix=$CROSS_PREFIX")
    # Tell FFmpeg to use the host pkg-config instead of falling back to 'false'
    # when it doesn't find prefixed-pkg-config (e.g., aarch64-linux-gnu-pkg-config).
    CONFIGURE_ARGS+=("--pkg-config=pkg-config")
fi

if [[ "$FF_OS" == "android" ]]; then
    # add ANDROID_NDK_HOME to the search path for the Android sysroot headers/libraries
    CONFIGURE_ARGS+=("--sysroot=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/sysroot")
    CC="aarch64-linux-android26-clang"
elif [[ -n "${CLANG_TRIPLE:-}" ]]; then
    _CLANG_FLAGS="--target=$CLANG_TRIPLE"
    [[ -n "${CLANG_GCC_TOOLCHAIN:-}" ]] && _CLANG_FLAGS="$_CLANG_FLAGS --gcc-toolchain=$CLANG_GCC_TOOLCHAIN"
    CC="clang $_CLANG_FLAGS"
else
    CC="clang"
fi

CONFIGURE_ARGS+=("--cc=$CC")

# Let pkg-config find the just-built dav1d.
export PKG_CONFIG_PATH="${DAV1D_INSTALL_DIR}/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"

echo "==> Configuring FFmpeg for target: $TARGET"
echo "    Install dir : $INSTALL_DIR"
echo "    Build dir   : $BUILD_DIR"
echo "    Configure   : ${CONFIGURE_ARGS[*]}"
echo "    Using CC    : $CC"
echo "    PKG_CONFIG_PATH: $PKG_CONFIG_PATH"

cd "$BUILD_DIR"
"${CONFIGURE_ARGS[@]}"

# ── Build & install ───────────────────────────────────────────────────────────

echo "==> Building FFmpeg (jobs=$JOBS)"
make -j"$JOBS"

echo "==> Installing FFmpeg to $INSTALL_DIR"
make install

echo "==> FFmpeg build complete."
echo "    Libraries: $(ls "$INSTALL_DIR/lib/"*.a 2>/dev/null | xargs -n1 basename | tr '\n' ' ')"
