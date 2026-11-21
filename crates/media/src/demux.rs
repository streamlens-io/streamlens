//! Container demuxing with libavformat: yields the first H.264/H.265 track
//! of a file as Annex B access units, one per picture.

use std::ffi::CString;
use std::path::Path;

use streamlens_types::Codec;

use crate::ffi::ffmpeg::*;
use crate::ffi::{av_error_string, AVERROR_EAGAIN, AVERROR_EOF, AV_NOPTS_VALUE};
use crate::Error;

/// One access unit as read from the container. Borrows the demuxer's packet;
/// valid until the next [`Demuxer::read`] call.
pub struct AccessUnit<'a> {
    pub data: &'a [u8],
    pub pts_ns: Option<i64>,
    pub dts_ns: Option<i64>,
    pub duration_ns: Option<i64>,
    /// The container marks this packet as a random access point.
    pub keyframe: bool,
    /// Fraction of the file consumed, when the size and packet position are
    /// known.
    pub progress: Option<f32>,
}

pub struct Demuxer {
    fmt: *mut AVFormatContext,
    bsf: *mut AVBSFContext,
    pkt: *mut AVPacket,
    stream: i32,
    time_base: AVRational,
    codec: Codec,
    file_size: i64,
    eof_sent: bool,
}

// The FFmpeg contexts are owned exclusively by this value.
unsafe impl Send for Demuxer {}

impl Demuxer {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let cpath = path_to_cstring(path)?;
        let mut d = Demuxer {
            fmt: std::ptr::null_mut(),
            bsf: std::ptr::null_mut(),
            pkt: std::ptr::null_mut(),
            stream: -1,
            time_base: AVRational { num: 0, den: 1 },
            codec: Codec::H264,
            file_size: -1,
            eof_sent: false,
        };
        // SAFETY: every pointer is checked before use; `d`'s Drop frees
        // whatever was allocated when an early return happens.
        unsafe {
            check(
                "avformat_open_input",
                avformat_open_input(
                    &mut d.fmt,
                    cpath.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                ),
            )?;
            check(
                "avformat_find_stream_info",
                avformat_find_stream_info(d.fmt, std::ptr::null_mut()),
            )?;

            // The first H.264/H.265 track wins; everything else is skipped
            // at the demuxer.
            let streams =
                std::slice::from_raw_parts((*d.fmt).streams, (*d.fmt).nb_streams as usize);
            for (i, &st) in streams.iter().enumerate() {
                let id = (*(*st).codecpar).codec_id;
                let codec = if id == AVCodecID_AV_CODEC_ID_H264 {
                    Some(Codec::H264)
                } else if id == AVCodecID_AV_CODEC_ID_HEVC {
                    Some(Codec::H265)
                } else {
                    None
                };
                match codec {
                    Some(c) if d.stream < 0 => {
                        d.stream = i as i32;
                        d.codec = c;
                        d.time_base = (*st).time_base;
                    }
                    _ => (*st).discard = AVDiscard_AVDISCARD_ALL,
                }
            }
            if d.stream < 0 {
                return Err(Error::NoVideoStream);
            }

            // Length-prefixed samples (MP4/MKV) become Annex B; Annex B
            // input passes through unchanged.
            let filter = av_bsf_get_by_name(match d.codec {
                Codec::H264 => c"h264_mp4toannexb".as_ptr(),
                Codec::H265 => c"hevc_mp4toannexb".as_ptr(),
            });
            if filter.is_null() {
                return Err(Error::Ffmpeg {
                    call: "av_bsf_get_by_name",
                    msg: "mp4toannexb filter not built".into(),
                });
            }
            check("av_bsf_alloc", av_bsf_alloc(filter, &mut d.bsf))?;
            let st = *(*d.fmt).streams.add(d.stream as usize);
            check(
                "avcodec_parameters_copy",
                avcodec_parameters_copy((*d.bsf).par_in, (*st).codecpar),
            )?;
            (*d.bsf).time_base_in = (*st).time_base;
            check("av_bsf_init", av_bsf_init(d.bsf))?;

            d.pkt = av_packet_alloc();
            if d.pkt.is_null() {
                return Err(Error::Ffmpeg {
                    call: "av_packet_alloc",
                    msg: "out of memory".into(),
                });
            }
            if !(*d.fmt).pb.is_null() {
                d.file_size = avio_size((*d.fmt).pb);
            }
        }
        Ok(d)
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Next access unit in decode order; `None` at the end of the stream.
    pub fn read(&mut self) -> Result<Option<AccessUnit<'_>>, Error> {
        // SAFETY: the contexts were initialized in `open` and are only used
        // from `&mut self`.
        unsafe {
            loop {
                av_packet_unref(self.pkt);
                match av_bsf_receive_packet(self.bsf, self.pkt) {
                    0 => return Ok(Some(self.access_unit())),
                    AVERROR_EOF => return Ok(None),
                    AVERROR_EAGAIN if !self.eof_sent => self.feed_filter()?,
                    AVERROR_EAGAIN => return Ok(None),
                    e => return Err(ffmpeg_error("av_bsf_receive_packet", e)),
                }
            }
        }
    }

    /// Reads packets until one of the selected track reaches the filter (or
    /// signals end of input to it).
    unsafe fn feed_filter(&mut self) -> Result<(), Error> {
        loop {
            match av_read_frame(self.fmt, self.pkt) {
                AVERROR_EOF => {
                    self.eof_sent = true;
                    return check(
                        "av_bsf_send_packet",
                        av_bsf_send_packet(self.bsf, std::ptr::null_mut()),
                    );
                }
                e if e < 0 => return Err(ffmpeg_error("av_read_frame", e)),
                _ if (*self.pkt).stream_index != self.stream => av_packet_unref(self.pkt),
                // The filter takes over the packet's reference.
                _ => return check("av_bsf_send_packet", av_bsf_send_packet(self.bsf, self.pkt)),
            }
        }
    }

    unsafe fn access_unit(&self) -> AccessUnit<'_> {
        let p = &*self.pkt;
        let data = if p.data.is_null() || p.size <= 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(p.data, p.size as usize)
        };
        let ns = |ts: i64| (ts != AV_NOPTS_VALUE).then(|| to_ns(ts, self.time_base));
        AccessUnit {
            data,
            pts_ns: ns(p.pts),
            dts_ns: ns(p.dts),
            duration_ns: (p.duration > 0).then(|| to_ns(p.duration, self.time_base)),
            keyframe: p.flags & AV_PKT_FLAG_KEY as i32 != 0,
            progress: (self.file_size > 0 && p.pos >= 0)
                .then(|| (p.pos as f64 / self.file_size as f64) as f32),
        }
    }
}

impl Drop for Demuxer {
    fn drop(&mut self) {
        // SAFETY: the free functions accept null and reset the pointers.
        unsafe {
            av_packet_free(&mut self.pkt);
            av_bsf_free(&mut self.bsf);
            avformat_close_input(&mut self.fmt);
        }
    }
}

fn to_ns(ts: i64, tb: AVRational) -> i64 {
    (ts as i128 * tb.num as i128 * 1_000_000_000 / tb.den.max(1) as i128) as i64
}

fn check(call: &'static str, ret: i32) -> Result<(), Error> {
    if ret < 0 {
        Err(ffmpeg_error(call, ret))
    } else {
        Ok(())
    }
}

fn ffmpeg_error(call: &'static str, code: i32) -> Error {
    Error::Ffmpeg {
        call,
        msg: av_error_string(code),
    }
}

#[cfg(unix)]
fn path_to_cstring(path: &Path) -> Result<CString, Error> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes()).map_err(|_| Error::InvalidPath)
}

// FFmpeg's file protocol takes UTF-8 paths on Windows.
#[cfg(not(unix))]
fn path_to_cstring(path: &Path) -> Result<CString, Error> {
    let s = path.to_str().ok_or(Error::InvalidPath)?;
    CString::new(s).map_err(|_| Error::InvalidPath)
}
