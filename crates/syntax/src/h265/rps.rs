//! HEVC reference picture set / DPB management (8.3.2 / 8.3.4).
//!
//! Analysis-level approximation: tracks DPB entries by POC; no actual decoded
//! sample storage. Reference lists are built from the ShortTermRefPicSet and
//! LT entries of each slice header.

use crate::h265::slice::{LtEntry, SliceType};
use crate::h265::sps::ShortTermRefPicSet;
use crate::h265::NalType;

#[derive(Debug, Clone)]
struct Pic {
    decode_index: u32,
    poc: i32,
    long_term: bool,
}

#[derive(Debug, Default)]
pub struct Dpb {
    pics: Vec<Pic>,
    decode_index: u32,
}

/// Reference picture sets derived for one picture (8.3.2).
struct RefPicSets {
    /// Short-term, before current: POCs in RefPicSetStCurrBefore.
    st_curr_before: Vec<i32>,
    /// Short-term, after current: POCs in RefPicSetStCurrAfter.
    st_curr_after: Vec<i32>,
    /// Short-term following (not used by current): for DPB retention.
    st_foll: Vec<i32>,
    /// Long-term used by current.
    lt_curr: Vec<i32>,
}

impl Dpb {
    /// Compute the reference picture sets for the current picture (8.3.2),
    /// update DPB, and build ref lists L0/L1.
    #[allow(clippy::too_many_arguments)]
    pub fn process(
        &mut self,
        nal_type: NalType,
        poc: i32,
        rps: &ShortTermRefPicSet,
        lt_entries: &[LtEntry],
        slice_type: SliceType,
        num_ref_idx_active: &[u32; 2],
        ref_pic_list_mod: &[Vec<u32>; 2],
        sps_max_dec_pic_buffering: u32,
        log2_max_poc_lsb: u8,
    ) -> [Vec<u32>; 2] {
        // Index of the picture being processed; bump only after storing it
        // (incrementing first shifted every stored index by one, so each
        // frame appeared to reference itself).
        let di = self.decode_index;
        self.decode_index += 1;

        // IDR / BLA clears DPB (8.3.1).
        if nal_type.is_idr()
            || matches!(
                nal_type,
                NalType::BlaWLp | NalType::BlaWRadl | NalType::BlaNLp
            )
        {
            self.pics.clear();
        }

        // Build reference picture sets (8.3.2).
        let sets = self.derive_rps(poc, rps, lt_entries, log2_max_poc_lsb);

        // Build default ref lists (8.3.4).
        let ref_lists =
            self.build_ref_lists(&sets, slice_type, num_ref_idx_active, ref_pic_list_mod);

        // Remove pictures not in any reference set (8.3.2 "no longer needed").
        self.evict(&sets, sps_max_dec_pic_buffering);

        // Add current picture to DPB as short-term reference.
        // (For sub-layer non-ref _N types, the picture is still stored for
        //  DPB management but won't be referenced across sub-layers.)
        self.pics.push(Pic {
            decode_index: di,
            poc,
            long_term: false,
        });

        ref_lists
    }

    fn derive_rps(
        &self,
        current_poc: i32,
        rps: &ShortTermRefPicSet,
        lt_entries: &[LtEntry],
        log2_max_poc_lsb: u8,
    ) -> RefPicSets {
        let max_poc_lsb = 1i32 << log2_max_poc_lsb;

        // StCurrBefore: delta_poc_s0 entries used_by_curr.
        let st_curr_before: Vec<i32> = rps
            .delta_poc_s0
            .iter()
            .zip(&rps.used_by_curr_s0)
            .filter(|&(_, &u)| u)
            .map(|(&d, _)| current_poc + d)
            .collect();

        // StCurrAfter: delta_poc_s1 entries used_by_curr.
        let st_curr_after: Vec<i32> = rps
            .delta_poc_s1
            .iter()
            .zip(&rps.used_by_curr_s1)
            .filter(|&(_, &u)| u)
            .map(|(&d, _)| current_poc + d)
            .collect();

        // StFoll: not used_by_curr entries (kept for inter-layer or future refs).
        let mut st_foll: Vec<i32> = rps
            .delta_poc_s0
            .iter()
            .zip(&rps.used_by_curr_s0)
            .filter(|&(_, &u)| !u)
            .map(|(&d, _)| current_poc + d)
            .collect();
        st_foll.extend(
            rps.delta_poc_s1
                .iter()
                .zip(&rps.used_by_curr_s1)
                .filter(|&(_, &u)| !u)
                .map(|(&d, _)| current_poc + d),
        );

        // LtCurr: long-term entries used_by_curr.
        // Resolve LSB-only POC by wrapping around log2_max_poc_lsb.
        let lt_curr: Vec<i32> = lt_entries
            .iter()
            .filter(|e| e.used_by_curr)
            .map(|e| {
                if e.delta_poc_msb_present {
                    // delta_poc_msb_cycle_lt gives the offset in units of MaxPicOrderCntLsb.
                    current_poc
                        - e.delta_poc_msb_cycle_lt as i32 * max_poc_lsb
                        - (current_poc & (max_poc_lsb - 1))
                        + e.poc_lsb as i32
                } else {
                    // Only LSB: find the DPB entry whose POC LSB matches.
                    let lsb = e.poc_lsb as i32;
                    self.pics
                        .iter()
                        .min_by_key(|p| ((p.poc & (max_poc_lsb - 1)) - lsb).abs())
                        .map(|p| p.poc)
                        .unwrap_or(lsb)
                }
            })
            .collect();

        RefPicSets {
            st_curr_before,
            st_curr_after,
            st_foll,
            lt_curr,
        }
    }

    fn build_ref_lists(
        &self,
        sets: &RefPicSets,
        slice_type: SliceType,
        num_ref_idx_active: &[u32; 2],
        ref_pic_list_mod: &[Vec<u32>; 2],
    ) -> [Vec<u32>; 2] {
        if matches!(slice_type, SliceType::I) {
            return [Vec::new(), Vec::new()];
        }

        // Default list construction (8.3.4).
        // L0: StCurrBefore, StCurrAfter, LtCurr, cycled.
        // L1: StCurrAfter, StCurrBefore, LtCurr, cycled.
        let poc_to_di = |poc: i32| -> Option<u32> {
            self.pics
                .iter()
                .find(|p| p.poc == poc)
                .map(|p| p.decode_index)
        };

        let before: Vec<u32> = sets
            .st_curr_before
            .iter()
            .filter_map(|&p| poc_to_di(p))
            .collect();
        let after: Vec<u32> = sets
            .st_curr_after
            .iter()
            .filter_map(|&p| poc_to_di(p))
            .collect();
        let lt: Vec<u32> = sets.lt_curr.iter().filter_map(|&p| poc_to_di(p)).collect();

        let build = |prio0: &[u32], prio1: &[u32], max: u32| -> Vec<u32> {
            let mut list: Vec<u32> = prio0.to_vec();
            list.extend_from_slice(prio1);
            list.extend_from_slice(&lt);
            // Cycle if list is shorter than num_ref_idx_active.
            if !list.is_empty() {
                let orig_len = list.len();
                while (list.len() as u32) < max {
                    let idx = list.len() % orig_len;
                    let v = list[idx];
                    list.push(v);
                }
            }
            list.truncate(max as usize);
            list
        };

        let n0 = num_ref_idx_active[0];
        let n1 = num_ref_idx_active[1];

        let mut l0 = build(&before, &after, n0);
        let mut l1 = if slice_type == SliceType::B {
            build(&after, &before, n1)
        } else {
            Vec::new()
        };

        // Apply ref_pic_list_modification (8.3.4.1).
        apply_rpl_mod(&mut l0, &ref_pic_list_mod[0], n0);
        if slice_type == SliceType::B {
            apply_rpl_mod(&mut l1, &ref_pic_list_mod[1], n1);
        }

        [l0, l1]
    }

    /// Remove DPB entries not in any reference set (8.3.2 "no longer needed").
    fn evict(&mut self, sets: &RefPicSets, max_dpb: u32) {
        let keep: Vec<i32> = sets
            .st_curr_before
            .iter()
            .chain(&sets.st_curr_after)
            .chain(&sets.st_foll)
            .chain(&sets.lt_curr)
            .copied()
            .collect();

        self.pics.retain(|p| keep.contains(&p.poc));

        // If still over limit, remove oldest short-term.
        while self.pics.len() as u32 >= max_dpb {
            if let Some(oldest) = self
                .pics
                .iter()
                .filter(|p| !p.long_term)
                .min_by_key(|p| p.decode_index)
                .map(|p| p.decode_index)
            {
                self.pics.retain(|p| p.decode_index != oldest);
            } else {
                break;
            }
        }
    }
}

/// Apply ref_pic_list_modification entries: each entry[i] is an index into the
/// combined (before+after+lt) pool; reorder `list` accordingly.
fn apply_rpl_mod(list: &mut Vec<u32>, mods: &[u32], num_active: u32) {
    if mods.is_empty() {
        return;
    }
    let pool: Vec<u32> = list.clone();
    list.resize(num_active as usize, 0);
    for (i, &idx) in mods.iter().enumerate() {
        if let Some(&di) = pool.get(idx as usize) {
            if i < list.len() {
                list[i] = di;
            }
        }
    }
}
