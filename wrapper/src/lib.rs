pub mod decoder;
mod ffi;

use decoder::{Codec, Decoder, DecoderError, PixelFormat};
use std::ffi::{c_void, CStr};
use std::ptr;

// ── Public C-compatible types ──────────────────────────────────────────────────

/// Supported codecs.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoyuVideoCodec {
    Vp9 = 0,
    Av1 = 1,
}

/// Supported output pixel formats.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoyuVideoPixelFormat {
    /// Planar YUV 4:2:0.  Planes: Y, U, V.
    I420 = 0,
    /// Semi-planar YUV 4:2:0.  Planes: Y, UV-interleaved.
    Nv12 = 1,
}

/// Return codes for all API functions.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoyuVideoResult {
    /// Operation completed successfully.
    Ok = 0,
    /// Unspecified internal error.
    Error = -1,
    /// The codec pipeline requires more input before producing output.
    Again = -2,
    /// The decoder has been flushed and there are no more frames to deliver.
    Eof = -3,
    /// An invalid argument was supplied.
    InvalidArgument = -4,
}

/// Configuration passed to `moyu_video_decoder_create`.
#[repr(C)]
pub struct MoyuVideoDecoderConfig {
    pub codec: MoyuVideoCodec,
    /// Number of decoding threads. 0 = automatic (based on CPU core count).
    pub thread_count: i32,
}

/// Decoded frame descriptor.
///
/// The plane pointers remain valid until the next call to
/// `moyu_video_decoder_receive_frame`, `moyu_video_decoder_flush`, or
/// `moyu_video_decoder_destroy`.  Callers that need to retain frame data past
/// that point must copy the plane contents themselves.
#[repr(C)]
pub struct MoyuVideoFrame {
    /// Plane data pointers.  For I420: [Y, U, V].  For NV12: [Y, UV, NULL].
    pub planes: [*const u8; 3],
    /// Row stride in bytes for each plane.
    pub strides: [i32; 3],
    pub width: i32,
    pub height: i32,
    pub format: MoyuVideoPixelFormat,
    /// Presentation timestamp, transparently forwarded from `send_packet`.
    pub pts: i64,
}

/// Opaque decoder handle.
pub struct MoyuVideoDecoder(Decoder);

// ── API functions ──────────────────────────────────────────────────────────────

/// Create and open a new decoder.
///
/// On success `*out_decoder` is set to a non-null handle and `MoyuVideoResult::Ok`
/// is returned.  The caller is responsible for eventually calling
/// `moyu_video_decoder_destroy`.
///
/// # Safety
/// `config` must be a valid non-null pointer to a `MoyuVideoDecoderConfig`.
/// `out_decoder` must be a valid non-null pointer to a `*mut MoyuVideoDecoder`.
#[no_mangle]
pub unsafe extern "C" fn moyu_video_decoder_create(
    config: *const MoyuVideoDecoderConfig,
    out_decoder: *mut *mut MoyuVideoDecoder,
) -> MoyuVideoResult {
    if config.is_null() || out_decoder.is_null() {
        return MoyuVideoResult::InvalidArgument;
    }

    let cfg = &*config;

    let codec = match cfg.codec {
        MoyuVideoCodec::Vp9 => Codec::Vp9,
        MoyuVideoCodec::Av1 => Codec::Av1,
    };

    match Decoder::new(codec, cfg.thread_count) {
        Ok(dec) => {
            *out_decoder = Box::into_raw(Box::new(MoyuVideoDecoder(dec)));
            MoyuVideoResult::Ok
        }
        Err(DecoderError::CodecNotFound) | Err(DecoderError::AllocationFailed) => {
            *out_decoder = ptr::null_mut();
            MoyuVideoResult::Error
        }
        Err(_) => {
            *out_decoder = ptr::null_mut();
            MoyuVideoResult::Error
        }
    }
}

/// Send a compressed video packet to the decoder.
///
/// `data` must point to at least `size` bytes of compressed bitstream data, or
/// both `data` and `size` may be 0/null to signal end-of-stream (flush).
/// `pts` is an opaque timestamp value that is preserved and returned with the
/// corresponding decoded frame.
///
/// # Safety
/// `decoder` must be a valid, non-null handle returned by `moyu_video_decoder_create`.
/// If `size > 0`, `data` must point to a readable buffer of at least `size` bytes.
#[no_mangle]
pub unsafe extern "C" fn moyu_video_decoder_send_packet(
    decoder: *mut MoyuVideoDecoder,
    data: *const u8,
    size: i32,
    pts: i64,
) -> MoyuVideoResult {
    if decoder.is_null() {
        return MoyuVideoResult::InvalidArgument;
    }

    let dec = &mut (*decoder).0;

    let payload = if data.is_null() || size <= 0 {
        None
    } else {
        Some(std::slice::from_raw_parts(data, size as usize))
    };

    match dec.send_packet(payload, pts) {
        Ok(()) => MoyuVideoResult::Ok,
        Err(DecoderError::Again) => MoyuVideoResult::Again,
        Err(_) => MoyuVideoResult::Error,
    }
}

/// Attempt to retrieve a decoded frame from the decoder.
///
/// On success `out_frame` is populated with pointers into decoder-owned memory
/// and `MoyuVideoResult::Ok` is returned.  Returns `MoyuVideoResult::Again` if
/// more input packets must be sent before a frame is available.  Returns
/// `MoyuVideoResult::Eof` after flushing when no more frames remain.
///
/// # Safety
/// `decoder` must be a valid, non-null handle returned by `moyu_video_decoder_create`.
/// `out_frame` must be a valid, non-null pointer writable by this function.
#[no_mangle]
pub unsafe extern "C" fn moyu_video_decoder_receive_frame(
    decoder: *mut MoyuVideoDecoder,
    out_frame: *mut MoyuVideoFrame,
) -> MoyuVideoResult {
    if decoder.is_null() || out_frame.is_null() {
        return MoyuVideoResult::InvalidArgument;
    }

    let dec = &mut (*decoder).0;

    match dec.receive_frame() {
        Ok(frame) => {
            let c_fmt = match frame.format {
                PixelFormat::I420 => MoyuVideoPixelFormat::I420,
                PixelFormat::Nv12 => MoyuVideoPixelFormat::Nv12,
            };
            *out_frame = MoyuVideoFrame {
                planes: frame.planes,
                strides: frame.strides,
                width: frame.width,
                height: frame.height,
                format: c_fmt,
                pts: frame.pts,
            };
            MoyuVideoResult::Ok
        }
        Err(DecoderError::Again) => MoyuVideoResult::Again,
        Err(DecoderError::Eof) => MoyuVideoResult::Eof,
        Err(_) => MoyuVideoResult::Error,
    }
}

/// Reset the decoder's internal buffers.
///
/// This function must be called after a seek operation to discard any buffered
/// state carried over from the previous playback position.
///
/// # Safety
/// `decoder` must be a valid, non-null handle returned by `moyu_video_decoder_create`.
#[no_mangle]
pub unsafe extern "C" fn moyu_video_decoder_flush(decoder: *mut MoyuVideoDecoder) {
    if !decoder.is_null() {
        (*decoder).0.flush();
    }
}

/// Destroy a decoder and release all associated resources.
///
/// The handle must not be used after this call.
///
/// # Safety
/// `decoder` must be a valid, non-null handle returned by `moyu_video_decoder_create`.
#[no_mangle]
pub unsafe extern "C" fn moyu_video_decoder_destroy(decoder: *mut MoyuVideoDecoder) {
    if !decoder.is_null() {
        let _ = Box::from_raw(decoder);
    }
}

/// Return FFmpeg version information as a null-terminated UTF-8 string.
///
/// The returned pointer is valid for the lifetime of the loaded library
/// (it points to static data inside FFmpeg). The caller must NOT free it.
///
/// The format is: `"FFmpeg <version>; libavcodec <major>.<minor>.<micro>; <configuration>"`
///
/// # Safety
/// The returned `*const c_char` points to a statically allocated string inside
/// the library.  It is safe to read from any thread and must not be freed.
#[no_mangle]
pub unsafe extern "C" fn moyu_video_get_ffmpeg_info() -> *const std::ffi::c_char {
    use std::sync::Once;

    static ONCE: Once = Once::new();
    static mut INFO: *const std::ffi::c_char = std::ptr::null();

    ONCE.call_once(|| {
        let version_ptr = ffi::av_version_info();
        let version = if version_ptr.is_null() {
            "unknown"
        } else {
            CStr::from_ptr(version_ptr).to_str().unwrap_or("unknown")
        };

        let raw_ver = ffi::avcodec_version();
        let major = (raw_ver >> 16) & 0xFF;
        let minor = (raw_ver >> 8) & 0xFF;
        let micro = raw_ver & 0xFF;

        let config_ptr = ffi::avcodec_configuration();
        let config = if config_ptr.is_null() {
            ""
        } else {
            CStr::from_ptr(config_ptr).to_str().unwrap_or("")
        };

        let mut codecs = vec![];
        let mut opaque: *mut c_void = ptr::null_mut();
        loop {
            let codec_ptr = ffi::av_codec_iterate(&mut opaque);
            if codec_ptr.is_null() {
                break;
            }
            let codec = &*codec_ptr;
            if ffi::av_codec_is_decoder(codec) != 0 {
                let name = CStr::from_ptr(codec.name).to_string_lossy().into_owned();
                codecs.push(name);
            }
        }

        let s = format!(
            "FFmpeg {}; libavcodec {}.{}.{}; {}\nCodecs: {}\0",
            version,
            major,
            minor,
            micro,
            config,
            codecs.join(", "),
        );
        let leaked = s.into_bytes().leak();
        INFO = leaked.as_ptr() as *const std::ffi::c_char;
    });

    INFO
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn testdata_dir() -> std::path::PathBuf {
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest.parent().unwrap().join("fixtures")
    }

    /// Parse an IVF file and return (fourcc, width, height, vec of raw frame bytes).
    /// IVF is a trivial format: 32-byte file header, then repeated (12-byte frame header + frame data).
    fn read_ivf_frames(path: &std::path::Path) -> (String, u16, u16, Vec<Vec<u8>>) {
        let mut file = std::fs::File::open(path)
            .unwrap_or_else(|e| panic!("Cannot open {}: {}", path.display(), e));
        let mut data = Vec::new();
        file.read_to_end(&mut data).unwrap();

        assert!(data.len() >= 32, "IVF file too short");
        assert_eq!(&data[0..4], b"DKIF", "Not an IVF file");

        let fourcc = String::from_utf8_lossy(&data[8..12]).to_string();
        let width = u16::from_le_bytes([data[12], data[13]]);
        let height = u16::from_le_bytes([data[14], data[15]]);

        let mut frames = Vec::new();
        let mut pos = 32; // skip file header
        while pos + 12 <= data.len() {
            let frame_size =
                u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                    as usize;
            // bytes 4..12 = timestamp (8 bytes), skip
            let frame_start = pos + 12;
            let frame_end = frame_start + frame_size;
            if frame_end > data.len() {
                break;
            }
            frames.push(data[frame_start..frame_end].to_vec());
            pos = frame_end;
        }

        (fourcc, width, height, frames)
    }

    /// Feed IVF frames to the decoder and verify the first successfully decoded frame.
    /// All frame data access happens while the decoder is still alive (Frame contains
    /// borrowed pointers into the decoder's internal AVFrame buffers).
    fn decode_and_verify_first_frame(video_path: &std::path::Path, codec: Codec) {
        let (_fourcc, _w, _h, frames) = read_ivf_frames(video_path);
        assert!(!frames.is_empty(), "No frames in IVF file");

        let mut dec = Decoder::new(codec, 1).expect("Failed to create decoder");

        for (idx, frame_data) in frames.iter().enumerate() {
            dec.send_packet(Some(frame_data), idx as i64)
                .unwrap_or_else(|e| panic!("send_packet failed at frame {idx}: {:?}", e));

            loop {
                match dec.receive_frame() {
                    Ok(frame) => {
                        assert!(
                            frame.width > 0 && frame.height > 0,
                            "Frame dimensions must be positive"
                        );
                        assert!(!frame.planes[0].is_null(), "Y plane must not be null");
                        assert!(matches!(
                            frame.format,
                            PixelFormat::I420 | PixelFormat::Nv12
                        ));

                        // Verify Y plane has actual pixel data (not all zeros).
                        // SAFETY: plane pointer is valid while decoder is alive.
                        let y_size = (frame.strides[0] * frame.height) as usize;
                        let y_slice =
                            unsafe { std::slice::from_raw_parts(frame.planes[0], y_size) };
                        assert!(
                            y_slice.iter().any(|&b| b != 0),
                            "Y plane must contain non-zero pixel data"
                        );
                        return;
                    }
                    Err(DecoderError::Again) => break,
                    Err(e) => panic!("receive_frame returned unexpected error: {:?}", e),
                }
            }
        }

        panic!("No frame was decoded from {}", video_path.display());
    }

    #[test]
    fn test_vp9_decode_first_frame() {
        let path = testdata_dir().join("test_vp9.ivf");
        decode_and_verify_first_frame(&path, Codec::Vp9);
    }

    #[test]
    fn test_av1_decode_first_frame() {
        let path = testdata_dir().join("test_av1.ivf");
        decode_and_verify_first_frame(&path, Codec::Av1);
    }

    #[test]
    fn test_decoder_create_destroy_via_c_api() {
        unsafe {
            let config = MoyuVideoDecoderConfig {
                codec: MoyuVideoCodec::Vp9,
                thread_count: 1,
            };
            let mut handle: *mut MoyuVideoDecoder = ptr::null_mut();
            let result = moyu_video_decoder_create(&config, &mut handle);
            assert_eq!(result, MoyuVideoResult::Ok);
            assert!(!handle.is_null());
            moyu_video_decoder_destroy(handle);
        }
    }

    #[test]
    fn test_null_pointer_safety() {
        unsafe {
            assert_eq!(
                moyu_video_decoder_create(ptr::null(), ptr::null_mut()),
                MoyuVideoResult::InvalidArgument
            );
            assert_eq!(
                moyu_video_decoder_send_packet(ptr::null_mut(), ptr::null(), 0, 0),
                MoyuVideoResult::InvalidArgument
            );
            assert_eq!(
                moyu_video_decoder_receive_frame(ptr::null_mut(), ptr::null_mut()),
                MoyuVideoResult::InvalidArgument
            );
            // flush and destroy with null pointer must not crash.
            moyu_video_decoder_flush(ptr::null_mut());
            moyu_video_decoder_destroy(ptr::null_mut());
        }
    }
}
