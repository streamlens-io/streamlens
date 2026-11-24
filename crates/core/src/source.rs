//! Reads access units back from the analyzed file: seek to a frame, then
//! read forward in decode order. After positioning, packets come in the
//! order the index pass saw them, so each one is either the next frame
//! (same file offset and size) or a packet that is no frame.

use streamlens_media::{AccessUnit, Demuxer};
use streamlens_syntax::analyzer::splice_at;
use streamlens_types::FrameRecord;

use crate::index::IndexHandle;
use crate::Error;

/// One frame's access unit as read from the file.
pub(crate) struct Au {
    pub index: u32,
    /// Non-frame packets (parameter sets only) read just before this frame.
    /// The decoder needs them in front of it; they are not part of the
    /// frame's own AU.
    pub carry: Vec<u8>,
    /// The frame's Annex B access unit, insertion applied.
    pub data: Vec<u8>,
}

pub(crate) struct AuReader {
    index: IndexHandle,
    demuxer: Demuxer,
    /// Decode index the next [`Self::next_au`] returns; `None` until
    /// positioned.
    next: Option<u32>,
    /// The target frame's AU, read while seeking.
    pending: Option<Vec<u8>>,
    /// Bytes of non-frame packets (parameter sets only) read since the last
    /// frame; they go in front of the next one.
    carry: Vec<u8>,
    /// Nothing was read or sought since the demuxer was opened.
    at_start: bool,
}

impl AuReader {
    pub fn open(index: IndexHandle) -> Result<Self, Error> {
        let demuxer = Demuxer::open(&index.get().source)?;
        Ok(Self {
            index,
            demuxer,
            next: None,
            pending: None,
            carry: Vec::new(),
            at_start: true,
        })
    }

    /// Positions the reader so that [`Self::next_au`] returns `target`.
    pub fn seek(&mut self, target: u32) -> Result<(), Error> {
        if self.next == Some(target) {
            return Ok(());
        }
        let index = self.index.get();
        let rec = &index.frames[target as usize];
        // A fresh demuxer stands at the first frame already; seeking there
        // costs a failed HLS seek and a reopen (below).
        if std::mem::take(&mut self.at_start) && target == 0 && self.scan_to(rec)? {
            return Ok(());
        }
        // A seek can fail (the HLS demuxer refuses timestamps before the
        // first packet it read, of any rendition) or land past the target
        // (imprecise timestamp index): then read from the start instead.
        let seeked = match self.demuxer.seek(rec.dts.or(rec.pts), rec.file_offset) {
            Ok(()) => true,
            Err(e) => {
                log::debug!("seek to frame {target} failed ({e}); reading from the start");
                false
            }
        };
        if !seeked || !self.scan_to(rec)? {
            if seeked {
                log::warn!("seek to frame {target} overshot; rescanning from the start");
            }
            self.demuxer = Demuxer::open(&index.source)?;
            if !self.scan_to(rec)? {
                return Err(Error::Pipeline(format!(
                    "frame {target} not found in the file"
                )));
            }
        }
        Ok(())
    }

    /// Reads forward until the packet of `target`. False when a packet past
    /// it shows up first.
    fn scan_to(&mut self, target: &FrameRecord) -> Result<bool, Error> {
        self.next = None;
        self.pending = None;
        // Whatever precedes the target after a seek can be a partial packet;
        // a keyframe that needs parameter sets has an insertion instead.
        self.carry.clear();
        loop {
            let Some(au) = self.demuxer.read()? else {
                return Ok(false);
            };
            if is_frame(&au, target) {
                self.pending = Some(au.data.to_vec());
                self.next = Some(target.decode_index);
                return Ok(true);
            }
            // Decode timestamps rise in decode order; without them (raw
            // Annex B) file positions do.
            let past = match (au.dts_ns, target.dts) {
                (Some(d), Some(t)) => d > t,
                _ => au.file_offset.is_some_and(|o| o > target.file_offset),
            };
            if past {
                return Ok(false);
            }
        }
    }

    /// The next frame in decode order.
    pub fn next_au(&mut self) -> Result<Au, Error> {
        let i = self
            .next
            .ok_or_else(|| Error::Pipeline("reader not positioned".into()))?;
        let index = self.index.get();
        let rec = index
            .frames
            .get(i as usize)
            .ok_or_else(|| Error::Pipeline(format!("frame {i} is not indexed yet")))?;
        let data = match self.pending.take() {
            Some(data) => data,
            None => loop {
                let au = self
                    .demuxer
                    .read()?
                    .ok_or_else(|| Error::Pipeline(format!("end of file before frame {i}")))?;
                if is_frame(&au, rec) {
                    break au.data.to_vec();
                }
                self.carry.extend_from_slice(au.data);
            },
        };
        let data = match index.insertions.get(&i) {
            Some(ins) => splice_at(&data, ins.at as usize, &ins.bytes),
            None => data,
        };
        self.next = Some(i + 1);
        Ok(Au {
            index: i,
            carry: std::mem::take(&mut self.carry),
            data,
        })
    }
}

/// Whether `au` is the packet of frame `rec`: same size, and the same DTS
/// where there is one (HLS positions come from the demuxer, not a file),
/// else the same file position (raw Annex B).
fn is_frame(au: &AccessUnit<'_>, rec: &FrameRecord) -> bool {
    au.coded_size == rec.size_bytes
        && match (au.dts_ns, rec.dts) {
            (Some(a), Some(b)) => a == b,
            _ => au.file_offset == Some(rec.file_offset),
        }
}
