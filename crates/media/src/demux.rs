//! Container demuxing with libavformat: yields the first H.264/H.265 track
//! of a file as Annex B access units, one per picture.

use streamlens_types::Codec;

use crate::annexb::{self, LengthPrefixed};
use crate::ffi::ffmpeg::*;
use crate::ffi::{av_error_string, AVERROR_EOF, AV_NOPTS_VALUE};
use crate::input::{self, Source};
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
    /// Start and length of the input in ns, for progress when the size is
    /// unknown or packet positions are not file positions (HLS).
    span_ns: Option<(i64, i64)>,
    format_name: String,
    /// Set for MP4/MKV/FLV-style tracks; `None` for Annex B tracks.
    length_prefixed: Option<LengthPrefixed>,
    /// Conversion buffer for length-prefixed samples.
    buf: Vec<u8>,
}

// The FFmpeg contexts are owned exclusively by this value.
unsafe impl Send for Demuxer {}

impl Demuxer {
    pub fn open(source: &Source) -> Result<Self, Error> {
        let cpath = source.to_cstring()?;
        let mut d = Demuxer {
            fmt: std::ptr::null_mut(),
            pkt: std::ptr::null_mut(),
            stream: -1,
            time_base: AVRational { num: 0, den: 1 },
            codec: Codec::H264,
            file_size: -1,
            span_ns: None,
            format_name: String::new(),
            length_prefixed: None,
            buf: Vec::new(),
        };
        // SAFETY: every pointer is checked before use; `d`'s Drop frees
        // whatever was allocated when an early return happens.
        unsafe {
            // Info level logs every HLS segment open.
            static QUIET: std::sync::Once = std::sync::Once::new();
            QUIET.call_once(|| av_log_set_level(AV_LOG_WARNING as i32));
            d.fmt = avformat_alloc_context();
            if d.fmt.is_null() {
                return Err(Error::Ffmpeg {
                    call: "avformat_alloc_context",
                    msg: "out of memory".into(),
                });
            }
            input::verify_tls(d.fmt);
            // HLS segments are often served without a file extension
            // (`item-00000001`), which the demuxer otherwise refuses.
            let mut options: *mut AVDictionary = std::ptr::null_mut();
            av_dict_set(&mut options, c"extension_picky".as_ptr(), c"0".as_ptr(), 0);
            let ret =
                avformat_open_input(&mut d.fmt, cpath.as_ptr(), std::ptr::null(), &mut options);
            av_dict_free(&mut options);
            check("avformat_open_input", ret)?;
            d.format_name = std::ffi::CStr::from_ptr((*(*d.fmt).iformat).name)
                .to_string_lossy()
                .split(',')
                .next()
                .unwrap_or_default()
                .to_string();
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
            // Packet positions of a format without byte seeking (HLS, DASH)
            // are offsets in a segment, not in the input that was opened.
            let byte_positions = (*(*d.fmt).iformat).flags & AVFMT_NO_BYTE_SEEK as i32 == 0;
            if byte_positions && !(*d.fmt).pb.is_null() {
                d.file_size = avio_size((*d.fmt).pb);
            }
            let (start, duration) = ((*d.fmt).start_time, (*d.fmt).duration);
            if duration > 0 {
                let ns = |t: i64| t * (1_000_000_000 / AV_TIME_BASE as i64);
                let start = if start == AV_NOPTS_VALUE {
                    0
                } else {
                    ns(start)
                };
                d.span_ns = Some((start, ns(duration)));
            }
        }
        Ok(d)
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// The demuxer's short name (`mov`, `matroska`, `mpegts`, `hls`, …).
    pub fn format_name(&self) -> &str {
        &self.format_name
    }

    /// Start time and duration of the input in ns, when the container
    /// reports a duration.
    pub fn span_ns(&self) -> Option<(i64, i64)> {
        self.span_ns
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
                    // Rounded to the nearest tick, which restores the
                    // packet's exact timestamp (ns came from it). Rounding
                    // down can land one tick before the stream start, which
                    // the HLS demuxer refuses.
                    let tb = self.time_base;
                    let per_tick = tb.num.max(1) as i128 * 1_000_000_000;
                    let ts =
                        (ns as i128 * tb.den as i128 + per_tick / 2).div_euclid(per_tick) as i64;
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
            progress: if self.file_size > 0 && p.pos >= 0 {
                Some((p.pos as f64 / self.file_size as f64) as f32)
            } else {
                // Unknown size (HLS): how far the timestamps are.
                self.span_ns.zip(ns(p.pts)).map(|((start, len), pts)| {
                    ((pts - start) as f64 / len as f64).clamp(0.0, 1.0) as f32
                })
            },
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
