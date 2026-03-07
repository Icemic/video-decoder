//! Integration test and usage example for `moyu_video`.
//!
//! This program loads the compiled dynamic library at runtime using `libloading`,
//! resolves all exported symbols, and exercises the full decode pipeline against
//! the VP9 and AV1 test fixture files located in `../fixtures/`.
//!
//! Build the library first:
//!
//! ```text
//! cargo build --release -p wrapper
//! ```
//!
//! Then run:
//!
//! ```text
//! MOYU_LIB=/path/to/libmoyu_video.so cargo run -p sample
//! ```

use libloading::{Library, Symbol};
use std::ffi::OsStr;
use std::io::Read;
use std::path::PathBuf;

// ── C type mirrors ────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MoyuVideoCodec {
    Vp9 = 0,
    Av1 = 1,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MoyuVideoPixelFormat {
    I420 = 0,
    #[allow(dead_code)]
    Nv12 = 1,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum MoyuVideoResult {
    Ok              =  0,
    Error           = -1,
    Again           = -2,
    Eof             = -3,
    InvalidArgument = -4,
}

#[repr(C)]
struct MoyuVideoDecoderConfig {
    codec: MoyuVideoCodec,
    output_format: MoyuVideoPixelFormat,
    thread_count: i32,
}

#[repr(C)]
struct MoyuVideoFrame {
    planes:  [*const u8; 3],
    strides: [i32; 3],
    width:   i32,
    height:  i32,
    format:  MoyuVideoPixelFormat,
    pts:     i64,
}

// Opaque handle type.
enum MoyuVideoDecoder {}

// ── Function pointer types ────────────────────────────────────────────────────

type FnDecoderCreate = unsafe extern "C" fn(
    *const MoyuVideoDecoderConfig,
    *mut *mut MoyuVideoDecoder,
) -> MoyuVideoResult;

type FnDecoderSendPacket = unsafe extern "C" fn(
    *mut MoyuVideoDecoder,
    *const u8,
    i32,
    i64,
) -> MoyuVideoResult;

type FnDecoderReceiveFrame = unsafe extern "C" fn(
    *mut MoyuVideoDecoder,
    *mut MoyuVideoFrame,
) -> MoyuVideoResult;

type FnDecoderFlush   = unsafe extern "C" fn(*mut MoyuVideoDecoder);
type FnDecoderDestroy = unsafe extern "C" fn(*mut MoyuVideoDecoder);
type FnGetFfmpegInfo  = unsafe extern "C" fn() -> *const std::ffi::c_char;

// ── Loaded library wrapper ────────────────────────────────────────────────────

struct WrapperLib {
    _lib:          Library,
    create:        FnDecoderCreate,
    send_packet:   FnDecoderSendPacket,
    receive_frame: FnDecoderReceiveFrame,
    flush:         FnDecoderFlush,
    destroy:       FnDecoderDestroy,
    get_ffmpeg_info: FnGetFfmpegInfo,
}

impl WrapperLib {
    fn load(path: &OsStr) -> Self {
        unsafe {
            let lib = Library::new(path)
                .unwrap_or_else(|e| panic!("Failed to load library {:?}: {}", path, e));

            let create: Symbol<FnDecoderCreate> = lib
                .get(b"moyu_video_decoder_create\0")
                .expect("Symbol moyu_video_decoder_create not found");
            let send_packet: Symbol<FnDecoderSendPacket> = lib
                .get(b"moyu_video_decoder_send_packet\0")
                .expect("Symbol moyu_video_decoder_send_packet not found");
            let receive_frame: Symbol<FnDecoderReceiveFrame> = lib
                .get(b"moyu_video_decoder_receive_frame\0")
                .expect("Symbol moyu_video_decoder_receive_frame not found");
            let flush: Symbol<FnDecoderFlush> = lib
                .get(b"moyu_video_decoder_flush\0")
                .expect("Symbol moyu_video_decoder_flush not found");
            let destroy: Symbol<FnDecoderDestroy> = lib
                .get(b"moyu_video_decoder_destroy\0")
                .expect("Symbol moyu_video_decoder_destroy not found");
            let get_ffmpeg_info: Symbol<FnGetFfmpegInfo> = lib
                .get(b"moyu_video_get_ffmpeg_info\0")
                .expect("Symbol moyu_video_get_ffmpeg_info not found");

            WrapperLib {
                create:          *create,
                send_packet:     *send_packet,
                receive_frame:   *receive_frame,
                flush:           *flush,
                destroy:         *destroy,
                get_ffmpeg_info: *get_ffmpeg_info,
                _lib:            lib,
            }
        }
    }
}

// ── Test helper ───────────────────────────────────────────────────────────────

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fixtures")
}

fn library_path() -> std::ffi::OsString {
    if let Ok(p) = std::env::var("MOYU_LIB") {
        return p.into();
    }

    // Default: look next to the binary (typical `cargo build --release` layout).
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap();

    #[cfg(target_os = "windows")]
    let name = "moyu_video.dll";
    #[cfg(target_os = "macos")]
    let name = "libmoyu_video.dylib";
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let name = "libmoyu_video.so";

    dir.join(name).into_os_string()
}

/// Parse an IVF file and return a vec of raw frame bytes.
fn read_ivf_frames(path: &std::path::Path) -> Vec<Vec<u8>> {
    let mut file = std::fs::File::open(path)
        .unwrap_or_else(|e| panic!("Cannot open {}: {}", path.display(), e));
    let mut data = Vec::new();
    file.read_to_end(&mut data).unwrap();

    assert!(data.len() >= 32, "IVF file too short");
    assert_eq!(&data[0..4], b"DKIF", "Not an IVF file");

    let mut frames = Vec::new();
    let mut pos = 32;
    while pos + 12 <= data.len() {
        let frame_size = u32::from_le_bytes([data[pos], data[pos+1], data[pos+2], data[pos+3]]) as usize;
        let frame_start = pos + 12;
        let frame_end = frame_start + frame_size;
        if frame_end > data.len() { break; }
        frames.push(data[frame_start..frame_end].to_vec());
        pos = frame_end;
    }
    frames
}

/// Feed IVF packets to the decoder and verify that frames are produced
/// with the expected properties.
fn run_decode_test(lib: &WrapperLib, video_path: &std::path::Path, codec: MoyuVideoCodec) {
    println!("Testing {:?} with file: {}", codec, video_path.display());

    let packets = read_ivf_frames(video_path);
    assert!(!packets.is_empty(), "No frames in IVF file");

    let config = MoyuVideoDecoderConfig {
        codec,
        output_format: MoyuVideoPixelFormat::I420,
        thread_count: 1,
    };
    let mut handle: *mut MoyuVideoDecoder = std::ptr::null_mut();
    let result = unsafe { (lib.create)(&config, &mut handle) };
    assert_eq!(result, MoyuVideoResult::Ok, "moyu_video_decoder_create failed");
    assert!(!handle.is_null());

    let mut frames_decoded = 0u32;

    'outer: for (idx, packet) in packets.iter().enumerate() {
        let r = unsafe {
            (lib.send_packet)(
                handle,
                packet.as_ptr(),
                packet.len() as i32,
                idx as i64,
            )
        };
        assert_eq!(r, MoyuVideoResult::Ok, "send_packet failed at frame {idx}");

        loop {
            let mut frame = MoyuVideoFrame {
                planes:  [std::ptr::null(); 3],
                strides: [0; 3],
                width:   0,
                height:  0,
                format:  MoyuVideoPixelFormat::I420,
                pts:     0,
            };

            let r = unsafe { (lib.receive_frame)(handle, &mut frame) };
            match r {
                MoyuVideoResult::Ok => {
                    frames_decoded += 1;
                    assert!(frame.width > 0,  "Frame width must be positive");
                    assert!(frame.height > 0, "Frame height must be positive");
                    assert!(!frame.planes[0].is_null(), "Y plane must not be null");
                    assert!(frame.strides[0] >= frame.width, "Y stride must be >= width");

                    println!(
                        "  Frame {frames_decoded}: {}x{} pts={}  strides=[{},{},{}]",
                        frame.width, frame.height, frame.pts,
                        frame.strides[0], frame.strides[1], frame.strides[2],
                    );

                    // Verify Y plane contains actual data (while decoder is alive).
                    let y_len = (frame.strides[0] * frame.height) as usize;
                    let y_slice = unsafe { std::slice::from_raw_parts(frame.planes[0], y_len) };
                    assert!(y_slice.iter().any(|&b| b != 0), "Y plane must contain non-zero pixel data");

                    if frames_decoded >= 3 {
                        break 'outer;
                    }
                }
                MoyuVideoResult::Again => break,
                MoyuVideoResult::Eof   => break 'outer,
                other => panic!("receive_frame returned unexpected result: {:?}", other),
            }
        }
    }

    assert!(frames_decoded >= 1, "At least one frame must be decoded");
    println!("  PASS — {frames_decoded} frame(s) decoded successfully.\n");

    unsafe { (lib.flush)(handle) };
    unsafe { (lib.destroy)(handle) };
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let lib_path = library_path();
    println!("Loading library: {:?}\n", lib_path);
    let lib = WrapperLib::load(&lib_path);

    // Print FFmpeg version info.
    let info_ptr = unsafe { (lib.get_ffmpeg_info)() };
    if !info_ptr.is_null() {
        let info = unsafe { std::ffi::CStr::from_ptr(info_ptr) };
        println!("FFmpeg info: {}\n", info.to_string_lossy());
    }

    let fixtures = fixtures_dir();
    run_decode_test(&lib, &fixtures.join("test_vp9.ivf"), MoyuVideoCodec::Vp9);
    run_decode_test(&lib, &fixtures.join("test_av1.ivf"),  MoyuVideoCodec::Av1);

    println!("All integration tests passed.");
}
