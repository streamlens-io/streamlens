//! Reads access units back from the analyzed file: seek to a frame, then
//! read forward in decode order. Packets are matched to frames by their
//! (file offset, size), which the index pass recorded.

use std::sync::Arc;

use streamlens_media::Demuxer;
use streamlens_syntax::analyzer::splice_at;

use crate::index::StreamIndex;
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
    index: Arc<StreamIndex>,
    demuxer: Demuxer,
    /// Decode index the next [`Self::next_au`] returns; `None` until
    /// positioned.
    next: Option<u32>,
    /// The target frame's AU, read while seeking.
    pending: Option<Vec<u8>>,
    /// Bytes of non-frame packets (parameter sets only) read since the last
    /// frame; they go in front of the next one.
    carry: Vec<u8>,
}

impl AuReader {
    pub fn open(index: Arc<StreamIndex>) -> Result<Self, Error> {
        let demuxer = Demuxer::open(&index.path)?;
        Ok(Self {
            index,
            demuxer,
            next: None,
            pending: None,
            carry: Vec::new(),
        })
    }

    /// Positions the reader so that [`Self::next_au`] returns `target`.
    pub fn seek(&mut self, target: u32) -> Result<(), Error> {
        if self.next == Some(target) {
            return Ok(());
        }
        let rec = &self.index.frames[target as usize];
        self.demuxer.seek(rec.dts.or(rec.pts), rec.file_offset)?;
        if !self.scan_to(target)? {
            // The seek landed past the target (imprecise timestamp index):
            // read from the start instead.
            log::warn!("seek to frame {target} overshot; rescanning from the start");
            self.demuxer = Demuxer::open(&self.index.path)?;
            if !self.scan_to(target)? {
                return Err(Error::Pipeline(format!(
                    "frame {target} not found in the file"
                )));
            }
        }
        Ok(())
    }

    /// Reads forward until the packet of `target`. False when a later frame
    /// shows up first.
    fn scan_to(&mut self, target: u32) -> Result<bool, Error> {
        self.next = None;
        self.pending = None;
        // Whatever precedes the target after a seek can be a partial packet;
        // a keyframe that needs parameter sets has an insertion instead.
        self.carry.clear();
        loop {
            let Some(au) = self.demuxer.read()? else {
                return Ok(false);
            };
            match self.index.frame_at(au.file_offset, au.coded_size) {
                Some(i) if i == target => {
                    self.pending = Some(au.data.to_vec());
                    self.next = Some(target);
                    return Ok(true);
                }
                Some(i) if i > target => return Ok(false),
                _ => {}
            }
        }
    }

    /// The next frame in decode order.
    pub fn next_au(&mut self) -> Result<Au, Error> {
        let i = self
            .next
            .ok_or_else(|| Error::Pipeline("reader not positioned".into()))?;
        let data = match self.pending.take() {
            Some(data) => data,
            None => loop {
                let au = self
                    .demuxer
                    .read()?
                    .ok_or_else(|| Error::Pipeline(format!("end of file before frame {i}")))?;
                match self.index.frame_at(au.file_offset, au.coded_size) {
                    Some(j) if j == i => break au.data.to_vec(),
                    Some(j) => {
                        return Err(Error::Pipeline(format!(
                            "expected frame {i} in the file, read frame {j}"
                        )))
                    }
                    None => self.carry.extend_from_slice(au.data),
                }
            },
        };
        let data = match self.index.insertions.get(&i) {
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
