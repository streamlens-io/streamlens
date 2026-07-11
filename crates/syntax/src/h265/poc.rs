//! Picture order count computation (8.3.1), HEVC.
//!
//! POC = PicOrderCntMsb + slice_pic_order_cnt_lsb.
//! PicOrderCntMsb is derived from the previous Tid0 picture.
//! For IRAP with NoRaslOutputFlag, msb resets to 0.

use crate::h265::NalType;

#[derive(Debug, Default)]
pub struct PocState {
    /// POC MSB of the previous temporal-id-0, non-RASL, non-RADL picture.
    prev_poc_msb: i32,
    /// POC LSB of that same picture.
    prev_poc_lsb: i32,
    /// Whether we have seen any non-IRAP picture yet (for CRA NoRasl handling).
    seen_non_irap: bool,
}

impl PocState {
    /// Compute PicOrderCntVal for a picture given its slice header LSB.
    ///
    /// `is_first_in_stream` should be true for the very first access unit,
    /// which starts a new CVS and triggers NoRaslOutputFlag=1 for CRA.
    pub fn compute(&mut self, nal_type: NalType, poc_lsb: u32, log2_max_poc_lsb: u8) -> i32 {
        let max_poc_lsb = 1i32 << log2_max_poc_lsb;
        let lsb = poc_lsb as i32;

        // 8.3.1: NoRaslOutputFlag = 1 for IDR, BLA, or first CRA in stream.
        let no_rasl = nal_type.is_idr()
            || matches!(
                nal_type,
                NalType::BlaWLp | NalType::BlaWRadl | NalType::BlaNLp
            )
            || (nal_type == NalType::Cra && !self.seen_non_irap);

        if nal_type.is_idr() {
            // IDR: POC = 0.
            // (We don't update prev_poc here; it is reset by noRasl below.)
            self.prev_poc_msb = 0;
            self.prev_poc_lsb = 0;
            self.seen_non_irap = false;
            return 0;
        }

        if no_rasl {
            // BLA or first CRA: msb forced to 0.
            let poc = lsb; // msb=0
            self.prev_poc_msb = 0;
            self.prev_poc_lsb = lsb;
            if !nal_type.is_irap() {
                self.seen_non_irap = true;
            }
            return poc;
        }

        // Normal MSB derivation (8.3.1).
        let msb = if lsb < self.prev_poc_lsb && self.prev_poc_lsb - lsb >= max_poc_lsb / 2 {
            self.prev_poc_msb + max_poc_lsb
        } else if lsb > self.prev_poc_lsb && lsb - self.prev_poc_lsb > max_poc_lsb / 2 {
            self.prev_poc_msb - max_poc_lsb
        } else {
            self.prev_poc_msb
        };

        let poc = msb + lsb;

        // Update prevTid0Pic: pictures with temporal_id == 0 that are not RASL/RADL
        // and not sub-layer non-reference (trailing non-ref).
        // The caller calls update_prev_tid0 separately when appropriate.

        if !nal_type.is_irap() {
            self.seen_non_irap = true;
        }

        poc
    }

    /// Call after compute() for pictures that qualify as prevTid0Pic:
    /// temporal_id==0, not RASL, not RADL, not sub-layer non-reference.
    pub fn update_prev_tid0(&mut self, poc: i32, poc_lsb: u32, log2_max_poc_lsb: u8) {
        let max_poc_lsb = 1i32 << log2_max_poc_lsb;
        let lsb = poc_lsb as i32;
        let msb = poc - lsb;
        // msb is already aligned; store so next picture can derive from it.
        // However if we crossed the boundary msb is already adjusted.
        // Simply store msb = poc & ~(max_poc_lsb-1) = poc - lsb.
        self.prev_poc_msb = poc - lsb;
        // Re-derive clean: msb * max_poc_lsb is what was added.
        // Actually the cleanest: prev_poc_msb = poc - (poc % max_poc_lsb) but
        // poc % max_poc_lsb is not always lsb (negative poc).
        // Use the simpler formula:
        self.prev_poc_msb = (poc / max_poc_lsb) * max_poc_lsb;
        self.prev_poc_lsb = msb.rem_euclid(max_poc_lsb);
        // Actually spec stores PicOrderCntMsb of prevTid0Pic:
        self.prev_poc_lsb = lsb;
        self.prev_poc_msb = poc - lsb;
    }
}
