//! Container demuxing with libavformat: yields the first H.264/H.265 track
//! of a file as Annex B access units, one per picture.

use std::ffi::CString;
use std::path::Path;

use streamlens_types::Codec;

use crate::annexb::{self, LengthPrefixed};
use crate::ffi::ffmpeg::*;
use crate::ffi::{av_error_string, AVERROR_EOF, AV_NOPTS_VALUE};
use crate::Error;

/// One access unit as read from the container. Borrows the demuxer;
/// valid until the next [`Demuxer::read`] call.
pub struct AccessUnit<'a> {
    /// Annex B bytes (length-prefixed samples are converted).
    pub data: &'a [u8],
    /// Size of the packet in the file, as stored (before conversion).
    pub coded_size: u32,
    /// Byte position of the packet in the file, when the demuxer knows it.
    pub file_offset: Option<u64>,
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
    pkt: *mut AVPacket,
    stream: i32,
    time_base: AVRational,
    codec: Codec,
    file_size: i64,
    /// Set for MP4/MKV/FLV-style tracks; `None` for Annex B tracks.
    length_prefixed: Option<LengthPrefixed>,
    /// Conversion buffer for length-prefixed samples.
    buf: Vec<u8>,
}

// The FFmpeg contexts are owned exclusively by this value.
unsafe impl Send for Demuxer {}

impl Demuxer {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let cpath = path_to_cstring(path)?;
        let mut d = Demuxer {
            fmt: std::ptr::null_mut(),
            pkt: std::ptr::null_mut(),
            stream: -1,
            time_base: AVRational { num: 0, den: 1 },
            codec: Codec::H264,
            file_size: -1,
            length_prefixed: None,
            buf: Vec::new(),
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

            let par = (*(*(*d.fmt).streams.add(d.stream as usize))).codecpar;
            let extradata = if (*par).extradata.is_null() || (*par).extradata_size <= 0 {
                &[][..]
            } else {
                std::slice::from_raw_parts((*par).extradata, (*par).extradata_size as usize)
            };
            d.length_prefixed = annexb::parse_config(d.codec, extradata)?;

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

    /// Parameter sets (and any SEI) the container keeps out of band in its
    /// decoder configuration record, as Annex B; empty for Annex B tracks,
    /// which carry them in-band. They apply to the whole track and are part
    /// of no access unit.
    pub fn config_nals(&self) -> &[u8] {
        self.length_prefixed
            .as_ref()
            .map_or(&[][..], |l| &l.config_nals)
    }

    /// Repositions so that reading continues at or before the packet at
    /// `file_offset`: by timestamp (the packet's DTS or PTS, in ns) when
    /// there is one, else by byte position. Seeks land on a random access
    /// point at or before the target; the caller reads forward from there.
    pub fn seek(&mut self, ts_ns: Option<i64>, file_offset: u64) -> Result<(), Error> {
        // SAFETY: the format context is valid; seeking only moves it.
        let ret = unsafe {
            match ts_ns {
                Some(ns) => {
                    // Rounded down so a backward seek cannot land past the
                    // target.
                    let tb = self.time_base;
                    let ts = (ns as i128 * tb.den as i128)
                        .div_euclid(tb.num.max(1) as i128 * 1_000_000_000)
                        as i64;
                    av_seek_frame(self.fmt, self.stream, ts, AVSEEK_FLAG_BACKWARD as i32)
                }
                None => av_seek_frame(self.fmt, -1, file_offset as i64, AVSEEK_FLAG_BYTE as i32),
            }
        };
        check("av_seek_frame", ret)
    }

    /// Next access unit in decode order; `None` at the end of the stream.
    pub fn read(&mut self) -> Result<Option<AccessUnit<'_>>, Error> {
        // SAFETY: the contexts were initialized in `open` and are only used
        // from `&mut self`.
        unsafe {
            loop {
                av_packet_unref(self.pkt);
                match av_read_frame(self.fmt, self.pkt) {
                    AVERROR_EOF => return Ok(None),
                    e if e < 0 => return Err(ffmpeg_error("av_read_frame", e)),
                    _ if (*self.pkt).stream_index != self.stream => continue,
                    _ => {}
                }
                let p = &*self.pkt;
                let packet = if p.data.is_null() || p.size <= 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(p.data, p.size as usize)
                };
                if let Some(lp) = &self.length_prefixed {
                    annexb::convert_sample(packet, lp.length_size, &mut self.buf)?;
                }
                return Ok(Some(self.access_unit(packet)));
            }
        }
    }

    unsafe fn access_unit<'a>(&'a self, packet: &'a [u8]) -> AccessUnit<'a> {
        let p = &*self.pkt;
        let data = if self.length_prefixed.is_some() {
            &self.buf[..]
        } else {
            packet
        };
        let ns = |ts: i64| (ts != AV_NOPTS_VALUE).then(|| to_ns(ts, self.time_base));
        AccessUnit {
            data,
            coded_size: packet.len() as u32,
            file_offset: (p.pos >= 0).then_some(p.pos as u64),
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
