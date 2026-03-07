# moyu\_video

A native dynamic library that wraps [FFmpeg](https://ffmpeg.org/) and exposes a minimal, stable C ABI for software decoding of VP9 and AV1 video streams.

## Purpose

`moyu_video` is designed for embedders (game engines, media players, streaming clients) that require deterministic, cross-platform, CPU-based video decoding without hardware-acceleration dependencies. The library:

- Accepts compressed video packets and produces raw pixel frames.
- Enforces an I420 (YUV420P) or NV12 output format, with explicit stride information on every frame, suitable for direct upload to GPU texture APIs such as wgpu, Metal, or Vulkan.
- Statically links a heavily stripped FFmpeg build, making the resulting binary fully self-contained with no runtime FFmpeg installation required.
- Exports only standard C ABI symbols; all Rust and FFmpeg internals are hidden.

## Supported Platforms

| Platform       | Architecture | Artifact                   |
|----------------|--------------|----------------------------|
| Linux          | x86\_64      | `libmoyu_video.so` |
| Linux          | aarch64      | `libmoyu_video.so` |
| Windows        | x86\_64      | `moyu_video.dll`   |
| macOS          | x86\_64      | `libmoyu_video.dylib` |
| macOS          | aarch64        | `libmoyu_video.dylib` |
| Android        | aarch64        | `libmoyu_video.so` |
| iOS            | aarch64        | `libmoyu_video.dylib` |

## Supported Codecs

- **VP9** (native FFmpeg software decoder)
- **AV1** (via [dav1d](https://code.videolan.org/videolan/dav1d), integrated through FFmpeg's libdav1d wrapper)

Hardware acceleration and GPU memory paths are explicitly excluded to guarantee consistent behaviour across all supported targets.

## C API

The public header is located at [`wrapper/include/moyu_video.h`](wrapper/include/moyu_video.h).

### Data types

```c
/* Codec selection */
typedef enum { MOYU_VIDEO_CODEC_VP9 = 0, MOYU_VIDEO_CODEC_AV1 = 1 } MoyuVideoCodec;

/* Output pixel format */
typedef enum {
    MOYU_VIDEO_PIXEL_FORMAT_I420 = 0,
    MOYU_VIDEO_PIXEL_FORMAT_NV12 = 1,
} MoyuVideoPixelFormat;

/* Return codes */
typedef enum {
    MOYU_VIDEO_OK               =  0,
    MOYU_VIDEO_ERROR            = -1,
    MOYU_VIDEO_AGAIN            = -2,
    MOYU_VIDEO_EOF              = -3,
    MOYU_VIDEO_INVALID_ARGUMENT = -4,
} MoyuVideoResult;

/* Decoder configuration */
typedef struct {
    MoyuVideoCodec       codec;
    MoyuVideoPixelFormat output_format;
    int32_t              thread_count; /* 0 = auto */
} MoyuVideoDecoderConfig;

/* Decoded frame descriptor */
typedef struct {
    const uint8_t       *planes[3];
    int32_t              strides[3];
    int32_t              width;
    int32_t              height;
    MoyuVideoPixelFormat format;
    int64_t              pts;
} MoyuVideoFrame;

typedef struct MoyuVideoDecoder MoyuVideoDecoder; /* opaque */
```

### Functions

```c
MoyuVideoResult moyu_video_decoder_create(
    const MoyuVideoDecoderConfig *config,
    MoyuVideoDecoder            **out_decoder
);

MoyuVideoResult moyu_video_decoder_send_packet(
    MoyuVideoDecoder *decoder,
    const uint8_t    *data,
    int32_t           size,
    int64_t           pts
);

MoyuVideoResult moyu_video_decoder_receive_frame(
    MoyuVideoDecoder *decoder,
    MoyuVideoFrame   *out_frame
);

void moyu_video_decoder_flush(MoyuVideoDecoder *decoder);
void moyu_video_decoder_destroy(MoyuVideoDecoder *decoder);

const char *moyu_video_get_ffmpeg_info(void);
```

### Usage pattern

```c
#include "moyu_video.h"

/* 1. Create a decoder */
MoyuVideoDecoderConfig config = {
    .codec         = MOYU_VIDEO_CODEC_VP9,
    .output_format = MOYU_VIDEO_PIXEL_FORMAT_I420,
    .thread_count  = 0,
};
MoyuVideoDecoder *dec = NULL;
assert(moyu_video_decoder_create(&config, &dec) == MOYU_VIDEO_OK);

/* 2. Feed packets (obtained from your demuxer) */
MoyuVideoResult r = moyu_video_decoder_send_packet(dec, pkt_data, pkt_size, pts);
assert(r == MOYU_VIDEO_OK);

/* 3. Drain frames */
MoyuVideoFrame frame;
while (moyu_video_decoder_receive_frame(dec, &frame) == MOYU_VIDEO_OK) {
    /* frame.planes[0] = Y, frame.planes[1] = U, frame.planes[2] = V */
    /* frame.strides[i] = bytes per row for plane i */
    upload_to_gpu(frame.planes, frame.strides, frame.width, frame.height);
}

/* 4. After a seek */
moyu_video_decoder_flush(dec);

/* 5. Flush at end of stream */
moyu_video_decoder_send_packet(dec, NULL, 0, 0);
while (moyu_video_decoder_receive_frame(dec, &frame) == MOYU_VIDEO_OK) { /* ... */ }

/* 6. Destroy */
moyu_video_decoder_destroy(dec);
```

### Frame memory lifetime

The `planes` pointers inside `MoyuVideoFrame` point into decoder-owned memory. They remain valid only until the next call to `moyu_video_decoder_receive_frame`, `moyu_video_decoder_flush`, or `moyu_video_decoder_destroy` on the same handle. Copy the pixel data if it must be retained longer.

## Building from Source

### Prerequisites

- Rust stable (≥ 1.78)
- A C compiler (gcc or clang) and GNU `make`
- `nasm` (required by FFmpeg's x86 assembly routines)
- `meson` and `ninja` (required to build dav1d)
- `bindgen` requires `libclang`; install with `apt install libclang-dev` or `brew install llvm`

The FFmpeg and dav1d sources are included as Git submodules. Initialize them if you have not done so:

```sh
git submodule update --init --recursive
```

### Native build (Linux / macOS)

```sh
cargo build --release -p wrapper
```

The `build.rs` script will configure, compile, and install dav1d (via meson/ninja) and a minimal FFmpeg into `$OUT_DIR` automatically on the first build. Subsequent builds reuse the cached installation unless source files change.

### Cross-compilation

Use the provided shell script to compile FFmpeg for the target platform, then set `FFMPEG_INSTALL_DIR` before building:

```sh
# Example: Linux aarch64
export CC=aarch64-linux-gnu-gcc
bash scripts/build_ffmpeg.sh \
    --target aarch64-unknown-linux-gnu \
    --install-dir "$PWD/ffmpeg_install_aarch64"

FFMPEG_INSTALL_DIR="$PWD/ffmpeg_install_aarch64" \
cargo build --release --target aarch64-unknown-linux-gnu -p wrapper
```

## Running Tests

### Unit tests

Unit tests are embedded in the `wrapper` crate and exercise both the Rust-layer decoder and the exported C API. They use IVF-format test fixtures in `fixtures/`:

```sh
# Generate test fixtures from a source video (IVF format)
ffmpeg -y -i source.mp4 -c:v libvpx-vp9 -b:v 1M -an -f ivf fixtures/test_vp9.ivf
ffmpeg -y -i source.mp4 -c:v libsvtav1 -crf 35 -preset 8 -an -f ivf fixtures/test_av1.ivf

cargo test -p wrapper
```

### Integration test

The `sample` binary loads the compiled dynamic library at runtime and validates the full C API pipeline:

```sh
# Build the library first
cargo build --release -p wrapper

# Run the integration test
MOYU_LIB=target/release/libmoyu_video.so cargo run -p sample
```

On macOS replace `.so` with `.dylib`; on Windows use `.dll`.

## CI / CD

GitHub Actions automatically builds the library for all supported targets on every push to `main` and on every pull request. Tagged commits (`v*`) additionally create a GitHub Release with all artifacts attached.

The workflow is defined in [`.github/workflows/build.yml`](.github/workflows/build.yml).

## License

This project is licensed under the **GNU Lesser General Public License, version 2.1 or later** (LGPL-2.1-or-later).

FFmpeg is also licensed under the LGPL-2.1-or-later (in its default configuration). This project enables only LGPL-compatible FFmpeg components; no GPL components are activated. dav1d is licensed under the BSD 2-Clause license.

See [`LICENSE`](LICENSE) for the full license text.
