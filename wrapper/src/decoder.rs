use std::ptr;

use libc;

use crate::ffi::*;

/// Supported codec identifiers, mirroring the public C enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Vp9,
    Av1,
}

/// Supported output pixel formats, mirroring the public C enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    I420,
    Nv12,
}

impl PixelFormat {
    fn as_av_pix_fmt(self) -> AVPixelFormat {
        match self {
            PixelFormat::I420 => AV_PIX_FMT_YUV420P,
            PixelFormat::Nv12 => AV_PIX_FMT_NV12,
        }
    }
}

/// Decoded frame data returned to the C caller.
/// The plane pointers refer to memory owned by the `Decoder`.
pub struct Frame {
    pub planes: [*const u8; 3],
    pub strides: [i32; 3],
    pub width: i32,
    pub height: i32,
    pub format: PixelFormat,
    pub pts: i64,
}

/// Errors that the decoder can return.
#[derive(Debug)]
pub enum DecoderError {
    /// The operation completed but no output is available yet; more input is required.
    Again,
    /// The decoder has been flushed and has no more frames to deliver.
    Eof,
    /// Codec not found or not supported.
    CodecNotFound,
    /// Context allocation failed.
    AllocationFailed,
    /// An FFmpeg API returned an unexpected error code.
    FfmpegError(i32),
    /// Invalid argument supplied by the caller.
    InvalidArgument,
}

/// Primary decoder state. All FFmpeg resources are owned here.
pub struct Decoder {
    codec_ctx: *mut AVCodecContext,
    packet: *mut AVPacket,
    /// Raw decoded frame from avcodec_receive_frame.
    av_frame: *mut AVFrame,
    /// Pixel-format-converted output frame.
    dst_frame: *mut AVFrame,
    /// libswscale context, created lazily on first frame.
    sws_ctx: *mut SwsContext,
    /// Source pixel format of the currently cached sws_ctx.
    sws_src_fmt: AVPixelFormat,
    output_format: PixelFormat,
    /// Backing buffer for dst_frame planes.
    dst_buf: Vec<u8>,
}

// SAFETY: All raw pointer operations are performed from a single thread at a
// time; the Decoder is not internally shared.
unsafe impl Send for Decoder {}

impl Decoder {
    /// Create and open a new software decoder for the specified codec.
    ///
    /// `thread_count` of 0 instructs FFmpeg to choose automatically based on
    /// the number of available CPU cores.
    pub fn new(codec: Codec, output_format: PixelFormat, thread_count: i32) -> Result<Self, DecoderError> {
        unsafe {
            let codec_id = match codec {
                Codec::Vp9 => AV_CODEC_ID_VP9,
                Codec::Av1 => AV_CODEC_ID_AV1,
            };

            let av_codec = avcodec_find_decoder(codec_id);
            if av_codec.is_null() {
                return Err(DecoderError::CodecNotFound);
            }

            let codec_ctx = avcodec_alloc_context3(av_codec);
            if codec_ctx.is_null() {
                return Err(DecoderError::AllocationFailed);
            }

            // Configure multi-threaded frame-level decoding.
            (*codec_ctx).thread_count = thread_count;
            (*codec_ctx).thread_type = FF_THREAD_FRAME as i32;

            let mut codec_ctx = codec_ctx;
            let ret = avcodec_open2(codec_ctx, av_codec, ptr::null_mut());
            if ret < 0 {
                avcodec_free_context(&mut codec_ctx);
                return Err(DecoderError::FfmpegError(ret));
            }

            let packet = av_packet_alloc();
            if packet.is_null() {
                avcodec_free_context(&mut codec_ctx);
                return Err(DecoderError::AllocationFailed);
            }
            let mut packet = packet;

            let av_frame = av_frame_alloc();
            if av_frame.is_null() {
                av_packet_free(&mut packet);
                avcodec_free_context(&mut codec_ctx);
                return Err(DecoderError::AllocationFailed);
            }
            let mut av_frame = av_frame;

            let dst_frame = av_frame_alloc();
            if dst_frame.is_null() {
                av_frame_free(&mut av_frame);
                av_packet_free(&mut packet);
                avcodec_free_context(&mut codec_ctx);
                return Err(DecoderError::AllocationFailed);
            }

            Ok(Decoder {
                codec_ctx,
                packet,
                av_frame,
                dst_frame,
                sws_ctx: ptr::null_mut(),
                sws_src_fmt: AV_PIX_FMT_NONE,
                output_format,
                dst_buf: Vec::new(),
            })
        }
    }

    /// Send a compressed packet to the decoder.
    ///
    /// Pass `data = None` to signal end-of-stream and flush the decoder.
    pub fn send_packet(&mut self, data: Option<&[u8]>, pts: i64) -> Result<(), DecoderError> {
        unsafe {
            match data {
                None => {
                    // Flush: send a null packet.
                    let ret = avcodec_send_packet(self.codec_ctx, ptr::null());
                    if ret < 0 && ret != averror_eof() {
                        return Err(DecoderError::FfmpegError(ret));
                    }
                }
                Some(bytes) => {
                    (*self.packet).data = bytes.as_ptr() as *mut u8;
                    (*self.packet).size = bytes.len() as i32;
                    (*self.packet).pts = pts;
                    (*self.packet).dts = pts;

                    let ret = avcodec_send_packet(self.codec_ctx, self.packet);
                    // Reset packet to avoid dangling references.
                    (*self.packet).data = ptr::null_mut();
                    (*self.packet).size = 0;

                    if ret < 0 {
                        if ret == AVERROR(EAGAIN as i32) {
                            return Err(DecoderError::Again);
                        }
                        return Err(DecoderError::FfmpegError(ret));
                    }
                }
            }
        }
        Ok(())
    }

    /// Attempt to receive a decoded frame from the decoder.
    ///
    /// Returns a reference into internally-owned memory that is valid until the
    /// next call to `receive_frame`, `flush`, or `drop`.
    pub fn receive_frame(&mut self) -> Result<Frame, DecoderError> {
        unsafe {
            av_frame_unref(self.av_frame);

            let ret = avcodec_receive_frame(self.codec_ctx, self.av_frame);
            if ret == AVERROR(EAGAIN as i32) {
                return Err(DecoderError::Again);
            }
            if ret == averror_eof() {
                return Err(DecoderError::Eof);
            }
            if ret < 0 {
                return Err(DecoderError::FfmpegError(ret));
            }

            let src_fmt = (*self.av_frame).format as AVPixelFormat;
            let width = (*self.av_frame).width;
            let height = (*self.av_frame).height;
            let pts = (*self.av_frame).pts;
            let dst_fmt = self.output_format.as_av_pix_fmt();

            // If the codec already outputs the requested format, return plane
            // pointers directly from the decoded frame (zero-copy path).
            if src_fmt == dst_fmt {
                let mut planes = [ptr::null::<u8>(); 3];
                let mut strides = [0i32; 3];
                let n_planes = num_planes(dst_fmt);
                for i in 0..n_planes {
                    planes[i] = (*self.av_frame).data[i] as *const u8;
                    strides[i] = (*self.av_frame).linesize[i];
                }
                return Ok(Frame { planes, strides, width, height, format: self.output_format, pts });
            }

            // Otherwise perform a pixel-format conversion via libswscale.
            self.ensure_sws_ctx(src_fmt, dst_fmt, width, height)?;
            self.ensure_dst_buf(dst_fmt, width, height)?;

            // Point dst_frame planes into our pre-allocated buffer.
            self.setup_dst_frame_planes(dst_fmt, width, height);

            sws_scale(
                self.sws_ctx,
                (*self.av_frame).data.as_ptr() as *const *const u8,
                (*self.av_frame).linesize.as_ptr(),
                0,
                height,
                (*self.dst_frame).data.as_ptr() as *mut *mut u8,
                (*self.dst_frame).linesize.as_ptr() as *mut i32,
            );

            let mut planes = [ptr::null::<u8>(); 3];
            let mut strides = [0i32; 3];
            let n_planes = num_planes(dst_fmt);
            for i in 0..n_planes {
                planes[i] = (*self.dst_frame).data[i] as *const u8;
                strides[i] = (*self.dst_frame).linesize[i];
            }

            Ok(Frame { planes, strides, width, height, format: self.output_format, pts })
        }
    }

    /// Flush decoder state (e.g. after seeking). The decoder is reset to its
    /// initial open state and is ready to accept new packets.
    pub fn flush(&mut self) {
        unsafe {
            avcodec_flush_buffers(self.codec_ctx);
        }
    }

    // ---------- private helpers ----------

    unsafe fn ensure_sws_ctx(
        &mut self,
        src_fmt: AVPixelFormat,
        dst_fmt: AVPixelFormat,
        width: i32,
        height: i32,
    ) -> Result<(), DecoderError> {
        // Recreate if dimensions or formats changed.
        let needs_new = if self.sws_ctx.is_null() {
            true
        } else {
            self.sws_src_fmt != src_fmt
                || (*self.dst_frame).width != width
                || (*self.dst_frame).height != height
                || (*self.dst_frame).format != dst_fmt as i32
        };

        if needs_new {
            if !self.sws_ctx.is_null() {
                sws_freeContext(self.sws_ctx);
            }
            self.sws_ctx = sws_getContext(
                width, height, src_fmt,
                width, height, dst_fmt,
                SWS_BILINEAR as i32,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            if self.sws_ctx.is_null() {
                return Err(DecoderError::AllocationFailed);
            }
            self.sws_src_fmt = src_fmt;
        }
        Ok(())
    }

    unsafe fn ensure_dst_buf(
        &mut self,
        fmt: AVPixelFormat,
        width: i32,
        height: i32,
    ) -> Result<(), DecoderError> {
        let needed = av_image_get_buffer_size(fmt, width, height, 1) as usize;
        if self.dst_buf.len() < needed
            || (*self.dst_frame).width != width
            || (*self.dst_frame).height != height
        {
            self.dst_buf.resize(needed, 0u8);
            (*self.dst_frame).width = width;
            (*self.dst_frame).height = height;
            (*self.dst_frame).format = fmt as i32;
        }
        Ok(())
    }

    unsafe fn setup_dst_frame_planes(&mut self, fmt: AVPixelFormat, width: i32, height: i32) {
        av_image_fill_arrays(
            (*self.dst_frame).data.as_mut_ptr(),
            (*self.dst_frame).linesize.as_mut_ptr(),
            self.dst_buf.as_ptr(),
            fmt,
            width,
            height,
            1,
        );
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            if !self.sws_ctx.is_null() {
                sws_freeContext(self.sws_ctx);
            }
            av_frame_free(&mut self.dst_frame);
            av_frame_free(&mut self.av_frame);
            av_packet_free(&mut self.packet);
            avcodec_free_context(&mut self.codec_ctx);
        }
    }
}

/// Return the number of planes for a given pixel format.
fn num_planes(fmt: AVPixelFormat) -> usize {
    if fmt == AV_PIX_FMT_NV12 {
        2
    } else {
        // YUV420P and most other planar formats use 3 planes.
        3
    }
}

// AVERROR macro emulation: AVERROR(e) == -e for POSIX error codes.
#[allow(non_snake_case)]
fn AVERROR(e: i32) -> i32 {
    -e
}

/// Emulate the FFmpeg `AVERROR_EOF` macro.  FFmpeg defines it as
/// `FFERRTAG( 'E','O','F',' ')` which expands to a specific tag value.
fn averror_eof() -> i32 {
    // FFERRTAG('E','O','F',' ') = -(0x45 | (0x4F << 8) | (0x46 << 16) | (0x20 << 24))
    -((0x45 | (0x4F << 8) | (0x46 << 16) | (0x20 << 24)) as i32)
}

const EAGAIN: u32 = libc::EAGAIN as u32;
