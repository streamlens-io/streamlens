//! Raw FFI bindings to the vendored decoders (generated at build time).

#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    unknown_lints,
    unnecessary_transmutes,
    clippy::all
)]
pub mod de265 {
    include!(concat!(env!("OUT_DIR"), "/de265_bindings.rs"));
}

#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    unknown_lints,
    unnecessary_transmutes,
    clippy::all
)]
pub mod ffmpeg {
    include!(concat!(env!("OUT_DIR"), "/ffmpeg_bindings.rs"));
}

/// AVERROR(EAGAIN) = -EAGAIN; the errno value is platform-dependent (11 on
/// Linux/Windows, 35 on macOS/BSD).
pub const AVERROR_EAGAIN: i32 = -libc::EAGAIN;
/// AVERROR_EOF = FFERRTAG('E','O','F',' ') = -MKTAG('E','O','F',' ').
pub const AVERROR_EOF: i32 = -0x2046_4F45;
/// AV_NOPTS_VALUE (libavutil/avutil.h): INT64_MIN.
pub const AV_NOPTS_VALUE: i64 = i64::MIN;

/// Human-readable message for an FFmpeg error code.
pub fn av_error_string(code: i32) -> String {
    let mut buf = [0 as std::ffi::c_char; 128];
    // SAFETY: av_strerror writes a NUL-terminated string into `buf`.
    unsafe {
        ffmpeg::av_strerror(code, buf.as_mut_ptr(), buf.len());
        std::ffi::CStr::from_ptr(buf.as_ptr())
            .to_string_lossy()
            .into_owned()
    }
}
