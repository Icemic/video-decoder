/**
 * moyu_video — Minimal VP9 / AV1 Software Decoder C API
 *
 * Copyright (C) 2026 the another-video-decoder contributors.
 * SPDX-License-Identifier: LGPL-2.1-or-later
 *
 * This library wraps FFmpeg's libavcodec and exposes a minimal, stable C ABI
 * for decoding VP9 and AV1 compressed video frames into raw YUV memory buffers.
 *
 * Usage pattern:
 *   1. Call moyu_video_decoder_create() once per stream.
 *   2. For each compressed packet, call moyu_video_decoder_send_packet().
 *   3. After each send, call moyu_video_decoder_receive_frame() in a loop
 *      until it returns MOYU_VIDEO_AGAIN.
 *   4. After a seek, call moyu_video_decoder_flush().
 *   5. To drain, call moyu_video_decoder_send_packet() with data=NULL/size=0,
 *      then drain with moyu_video_decoder_receive_frame() until it returns
 *      MOYU_VIDEO_EOF.
 *   6. Call moyu_video_decoder_destroy() when the decoder is no longer needed.
 */

#ifndef MOYU_VIDEO_H
#define MOYU_VIDEO_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── Enumerations ─────────────────────────────────────────────────────────── */

/** Supported codec identifiers. */
typedef enum {
    MOYU_VIDEO_CODEC_VP9 = 0,
    MOYU_VIDEO_CODEC_AV1 = 1,
} MoyuVideoCodec;

/** Supported output pixel formats. */
typedef enum {
    /** Planar YUV 4:2:0 (I420).  Three planes: Y, U, V. */
    MOYU_VIDEO_PIXEL_FORMAT_I420 = 0,
    /** Semi-planar YUV 4:2:0 (NV12).  Two planes: Y, interleaved UV. */
    MOYU_VIDEO_PIXEL_FORMAT_NV12 = 1,
} MoyuVideoPixelFormat;

/** Return codes produced by all API functions. */
typedef enum {
    /** Operation completed successfully. */
    MOYU_VIDEO_OK              =  0,
    /** Unspecified internal error. */
    MOYU_VIDEO_ERROR           = -1,
    /**
     * The codec pipeline requires more input before producing output.
     * Send additional packets and retry.
     */
    MOYU_VIDEO_AGAIN           = -2,
    /** The decoder has been flushed and there are no more frames to deliver. */
    MOYU_VIDEO_EOF             = -3,
    /** An invalid argument (e.g. a null pointer) was supplied. */
    MOYU_VIDEO_INVALID_ARGUMENT = -4,
} MoyuVideoResult;

/* ── Configuration ────────────────────────────────────────────────────────── */

/** Parameters passed to moyu_video_decoder_create(). */
typedef struct {
    MoyuVideoCodec       codec;
    MoyuVideoPixelFormat output_format;
    /**
     * Number of decoding threads.
     * 0 instructs the library to choose automatically based on the number of
     * available CPU cores.
     */
    int32_t thread_count;
} MoyuVideoDecoderConfig;

/* ── Frame descriptor ─────────────────────────────────────────────────────── */

/**
 * Decoded frame descriptor populated by moyu_video_decoder_receive_frame().
 *
 * Ownership: The plane pointers are valid only until the next call to
 * moyu_video_decoder_receive_frame(), moyu_video_decoder_flush(), or
 * moyu_video_decoder_destroy() on the same decoder handle.  Callers that need
 * to retain pixel data past that point must copy the contents into their own
 * buffer.
 */
typedef struct {
    /**
     * Pointers to plane data.
     *   - I420: planes[0]=Y  planes[1]=U  planes[2]=V
     *   - NV12: planes[0]=Y  planes[1]=UV (interleaved)  planes[2]=NULL
     */
    const uint8_t *planes[3];
    /** Row stride in bytes for each plane. */
    int32_t        strides[3];
    int32_t        width;
    int32_t        height;
    MoyuVideoPixelFormat format;
    /**
     * Presentation timestamp.  The value is the pts supplied by the caller in
     * the corresponding moyu_video_decoder_send_packet() call and is otherwise
     * opaque to the library.
     */
    int64_t        pts;
} MoyuVideoFrame;

/* ── Opaque handle ────────────────────────────────────────────────────────── */

/** Opaque decoder handle.  Do not dereference or copy. */
typedef struct MoyuVideoDecoder MoyuVideoDecoder;

/* ── API ──────────────────────────────────────────────────────────────────── */

/**
 * Create and open a new software decoder.
 *
 * @param config      Pointer to a fully initialised MoyuVideoDecoderConfig.
 * @param out_decoder On success, set to a newly allocated decoder handle.
 *                    The caller is responsible for calling
 *                    moyu_video_decoder_destroy() when finished.
 * @return            MOYU_VIDEO_OK on success; another value on failure.
 */
MoyuVideoResult moyu_video_decoder_create(
    const MoyuVideoDecoderConfig *config,
    MoyuVideoDecoder            **out_decoder
);

/**
 * Send a compressed video packet to the decoder.
 *
 * The provided buffer is consumed by this call; the caller may free it
 * immediately after the function returns.
 *
 * To signal end-of-stream, call this function with data=NULL and size=0.
 *
 * @param decoder  A valid decoder handle.
 * @param data     Pointer to the compressed bitstream bytes, or NULL to flush.
 * @param size     Number of bytes in @p data, or 0 to flush.
 * @param pts      Caller-defined presentation timestamp, passed through to the
 *                 corresponding MoyuVideoFrame.
 * @return         MOYU_VIDEO_OK on success.
 *                 MOYU_VIDEO_AGAIN if the codec's internal queue is full and
 *                 frames must be drained via moyu_video_decoder_receive_frame()
 *                 before more packets can be accepted.
 */
MoyuVideoResult moyu_video_decoder_send_packet(
    MoyuVideoDecoder *decoder,
    const uint8_t    *data,
    int32_t           size,
    int64_t           pts
);

/**
 * Attempt to retrieve a decoded frame.
 *
 * This function should be called in a loop after each moyu_video_decoder_send_packet()
 * until it returns MOYU_VIDEO_AGAIN, to ensure the internal pipeline is fully
 * drained.
 *
 * @param decoder    A valid decoder handle.
 * @param out_frame  On MOYU_VIDEO_OK, populated with the decoded frame data.
 * @return           MOYU_VIDEO_OK on success.
 *                   MOYU_VIDEO_AGAIN if no frame is available yet.
 *                   MOYU_VIDEO_EOF if the decoder has been flushed and
 *                   there are no more frames to deliver.
 */
MoyuVideoResult moyu_video_decoder_receive_frame(
    MoyuVideoDecoder *decoder,
    MoyuVideoFrame   *out_frame
);

/**
 * Reset the decoder's internal buffers.
 *
 * Must be called after a seek operation to discard decoder state accumulated
 * from the previous playback position.  The decoder can immediately accept new
 * packets after this call returns.
 *
 * @param decoder  A valid decoder handle.
 */
void moyu_video_decoder_flush(MoyuVideoDecoder *decoder);

/**
 * Destroy a decoder and release all associated resources.
 *
 * The handle must not be used after this call.
 *
 * @param decoder  A valid decoder handle.
 */
void moyu_video_decoder_destroy(MoyuVideoDecoder *decoder);

/**
 * Return a human-readable string describing the FFmpeg version, libavcodec
 * version, and build configuration linked into this library.
 *
 * The returned pointer is valid for the lifetime of the loaded library and
 * must NOT be freed by the caller.
 *
 * @return  A null-terminated UTF-8 string.
 */
const char *moyu_video_get_ffmpeg_info(void);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* MOYU_VIDEO_H */
