//! Decoded picture buffer emulation (8.2.4 reference list construction,
//! 8.2.5 marking) at syntax level: tracks which decoded pictures are
//! reference-eligible and builds the default reference lists per slice.
//!
//! Frame pictures only (field pairs are modeled as frames); this feeds the
//! BarChart reference arrows and `FrameRecord::ref_frames`.

use crate::h264::slice::{Mmco, RefListMod, SliceType};
use crate::h264::{SliceHeader, Sps};

#[derive(Debug, Clone)]
struct Pic {
    decode_index: u32,
    frame_num: u32,
    poc: i32,
    long_term: bool,
    long_term_idx: u32,
}

#[derive(Debug, Default)]
pub struct Dpb {
    pics: Vec<Pic>,
    max_long_term_idx_plus1: u32,
}

impl Dpb {
    /// Builds reference lists L0/L1 for a slice, before its picture is added.
    /// Returns decode-order indices, truncated to num_ref_idx_active.
    pub fn build_ref_lists(&self, sps: &Sps, sh: &SliceHeader, cur_poc: i32) -> [Vec<u32>; 2] {
        let max_frame_num = 1i64 << sps.log2_max_frame_num;
        let short: Vec<&Pic> = self.pics.iter().filter(|p| !p.long_term).collect();
        let long: Vec<&Pic> = {
            let mut v: Vec<&Pic> = self.pics.iter().filter(|p| p.long_term).collect();
            v.sort_by_key(|p| p.long_term_idx);
            v
        };

        let mut lists: [Vec<&Pic>; 2] = [Vec::new(), Vec::new()];
        match sh.slice_type {
            SliceType::P | SliceType::Sp => {
                // 8.2.4.2.1: short-term by FrameNumWrap descending.
                let mut s = short.clone();
                s.sort_by_key(|p| {
                    let wrap = if p.frame_num as i64 > sh.frame_num as i64 {
                        p.frame_num as i64 - max_frame_num
                    } else {
                        p.frame_num as i64
                    };
                    std::cmp::Reverse(wrap)
                });
                s.extend(long.iter().copied());
                lists[0] = s;
            }
            SliceType::B => {
                // 8.2.4.2.3: L0 = past desc, future asc; L1 = future asc, past desc.
                let mut past: Vec<&Pic> =
                    short.iter().copied().filter(|p| p.poc <= cur_poc).collect();
                let mut future: Vec<&Pic> =
                    short.iter().copied().filter(|p| p.poc > cur_poc).collect();
                past.sort_by_key(|p| std::cmp::Reverse(p.poc));
                future.sort_by_key(|p| p.poc);

                let mut l0 = past.clone();
                l0.extend(future.iter().copied());
                l0.extend(long.iter().copied());
                let mut l1 = future;
                l1.extend(past);
                l1.extend(long.iter().copied());
                // 8.2.4.2.3: if L1 == L0 and len > 1, swap its first two.
                if l1.len() > 1
                    && l0.len() == l1.len()
                    && l0
                        .iter()
                        .zip(&l1)
                        .all(|(a, b)| a.decode_index == b.decode_index)
                {
                    l1.swap(0, 1);
                }
                lists = [l0, l1];
            }
            SliceType::I | SliceType::Si => {}
        }

        let mut out: [Vec<u32>; 2] = [Vec::new(), Vec::new()];
        for (i, list) in lists.into_iter().enumerate() {
            let mut resolved: Vec<u32> = list.iter().map(|p| p.decode_index).collect();
            self.apply_modifications(
                &mut resolved,
                &list,
                &sh.ref_list_mods[i],
                sh,
                max_frame_num,
            );
            resolved.truncate(sh.num_ref_idx_active[i] as usize);
            out[i] = resolved;
        }
        out
    }

    /// 8.2.4.3 simplified: reorders `resolved` according to the modification
    /// operations (short-term picNum / long-term picNum addressing).
    fn apply_modifications(
        &self,
        resolved: &mut Vec<u32>,
        default_list: &[&Pic],
        mods: &[RefListMod],
        sh: &SliceHeader,
        max_frame_num: i64,
    ) {
        if mods.is_empty() {
            return;
        }
        let max_pic_num = max_frame_num;
        let mut pred_pic_num = sh.frame_num as i64;
        let mut insert_at = 0usize;
        for m in mods {
            let target: Option<u32> = match m.idc {
                0 | 1 => {
                    let diff = m.value as i64 + 1;
                    pred_pic_num = if m.idc == 0 {
                        (pred_pic_num - diff + max_pic_num) % max_pic_num
                    } else {
                        (pred_pic_num + diff) % max_pic_num
                    };
                    let want = pred_pic_num;
                    default_list
                        .iter()
                        .find(|p| !p.long_term && (p.frame_num as i64 % max_pic_num) == want)
                        .map(|p| p.decode_index)
                }
                2 => default_list
                    .iter()
                    .find(|p| p.long_term && p.long_term_idx == m.value)
                    .map(|p| p.decode_index),
                _ => None,
            };
            if let Some(idx) = target {
                resolved.retain(|&d| d != idx);
                let at = insert_at.min(resolved.len());
                resolved.insert(at, idx);
                insert_at += 1;
            }
        }
    }

    /// 8.2.5: marks the just-decoded picture and evicts per sliding window /
    /// MMCO. Call after `build_ref_lists`. Returns true if MMCO 5 occurred.
    pub fn mark_picture(
        &mut self,
        sps: &Sps,
        sh: &SliceHeader,
        decode_index: u32,
        poc: i32,
        is_ref: bool,
        is_idr: bool,
    ) -> bool {
        let mut mmco5 = false;
        if is_idr {
            self.pics.clear();
            if is_ref {
                let long_term = sh.idr_marking.map(|(_, lt)| lt).unwrap_or(false);
                self.pics.push(Pic {
                    decode_index,
                    frame_num: sh.frame_num,
                    poc,
                    long_term,
                    long_term_idx: 0,
                });
            }
            return false;
        }

        if is_ref {
            if sh.adaptive_ref_pic_marking {
                mmco5 = self.apply_mmcos(&sh.mmcos, sh, sps);
            } else {
                // Sliding window (8.2.5.3): evict oldest short-term when full.
                let num_short = self.pics.iter().filter(|p| !p.long_term).count();
                let num_long = self.pics.iter().filter(|p| p.long_term).count();
                if num_short + num_long >= sps.max_num_ref_frames.max(1) as usize {
                    if let Some(oldest) = self
                        .pics
                        .iter()
                        .filter(|p| !p.long_term)
                        .min_by_key(|p| self.frame_num_wrap(p, sh, sps))
                        .map(|p| p.decode_index)
                    {
                        self.pics.retain(|p| p.decode_index != oldest);
                    }
                }
            }
            self.pics.push(Pic {
                decode_index,
                frame_num: if mmco5 { 0 } else { sh.frame_num },
                poc: if mmco5 { 0 } else { poc },
                long_term: false,
                long_term_idx: 0,
            });
        }
        mmco5
    }

    fn frame_num_wrap(&self, p: &Pic, sh: &SliceHeader, sps: &Sps) -> i64 {
        let max = 1i64 << sps.log2_max_frame_num;
        if p.frame_num as i64 > sh.frame_num as i64 {
            p.frame_num as i64 - max
        } else {
            p.frame_num as i64
        }
    }

    fn apply_mmcos(&mut self, mmcos: &[Mmco], sh: &SliceHeader, sps: &Sps) -> bool {
        let max_frame_num = 1i64 << sps.log2_max_frame_num;
        let mut mmco5 = false;
        for m in mmcos {
            match m.op {
                1 => {
                    // Unmark short-term: picNum = frame_num - (diff+1).
                    let pic_num =
                        (sh.frame_num as i64 - (m.args[0] as i64 + 1)).rem_euclid(max_frame_num);
                    self.pics
                        .retain(|p| p.long_term || p.frame_num as i64 != pic_num);
                }
                2 => {
                    self.pics
                        .retain(|p| !(p.long_term && p.long_term_idx == m.args[0]));
                }
                3 => {
                    let pic_num =
                        (sh.frame_num as i64 - (m.args[0] as i64 + 1)).rem_euclid(max_frame_num);
                    for p in &mut self.pics {
                        if !p.long_term && p.frame_num as i64 == pic_num {
                            p.long_term = true;
                            p.long_term_idx = m.args[1];
                        }
                    }
                }
                4 => {
                    self.max_long_term_idx_plus1 = m.args[0];
                    let max = self.max_long_term_idx_plus1;
                    self.pics.retain(|p| !p.long_term || p.long_term_idx < max);
                }
                5 => {
                    self.pics.clear();
                    mmco5 = true;
                }
                6 => {
                    // Current picture becomes long-term; handled by caller's
                    // push (approximation: mark after push is not needed for
                    // analysis purposes).
                }
                _ => {}
            }
        }
        mmco5
    }
}
