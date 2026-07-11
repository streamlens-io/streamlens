//! Picture order count computation (8.2.1), frame pictures.
//!
//! Field pairs are treated as one frame picture: the reported POC is
//! TopFieldOrderCnt (equal to PicOrderCnt of the frame for display ordering).

use crate::h264::sps::PocType;
use crate::h264::{SliceHeader, Sps};

#[derive(Debug, Default)]
pub struct PocState {
    prev_pic_order_cnt_msb: i32,
    prev_pic_order_cnt_lsb: i32,
    prev_frame_num: u32,
    prev_frame_num_offset: u32,
}

impl PocState {
    /// Computes the POC of the picture whose first slice is `sh`.
    /// `is_ref` is `nal_ref_idc != 0`.
    pub fn compute(&mut self, sps: &Sps, sh: &SliceHeader, is_idr: bool, is_ref: bool) -> i32 {
        match &sps.poc {
            PocType::Type0 {
                log2_max_pic_order_cnt_lsb,
            } => {
                let max_lsb = 1i32 << log2_max_pic_order_cnt_lsb;
                if is_idr {
                    self.prev_pic_order_cnt_msb = 0;
                    self.prev_pic_order_cnt_lsb = 0;
                }
                let lsb = sh.pic_order_cnt_lsb as i32;
                let msb = if lsb < self.prev_pic_order_cnt_lsb
                    && self.prev_pic_order_cnt_lsb - lsb >= max_lsb / 2
                {
                    self.prev_pic_order_cnt_msb + max_lsb
                } else if lsb > self.prev_pic_order_cnt_lsb
                    && lsb - self.prev_pic_order_cnt_lsb > max_lsb / 2
                {
                    self.prev_pic_order_cnt_msb - max_lsb
                } else {
                    self.prev_pic_order_cnt_msb
                };
                if is_ref {
                    self.prev_pic_order_cnt_msb = msb;
                    self.prev_pic_order_cnt_lsb = lsb;
                }
                msb + lsb
            }
            PocType::Type1 {
                offset_for_non_ref_pic,
                offsets_for_ref_frame,
                ..
            } => {
                let max_frame_num = 1u32 << sps.log2_max_frame_num;
                let frame_num_offset = if is_idr {
                    0
                } else if self.prev_frame_num > sh.frame_num {
                    self.prev_frame_num_offset + max_frame_num
                } else {
                    self.prev_frame_num_offset
                };
                self.prev_frame_num_offset = frame_num_offset;
                self.prev_frame_num = sh.frame_num;

                let cycle = offsets_for_ref_frame;
                let mut abs_frame_num = if !cycle.is_empty() {
                    frame_num_offset + sh.frame_num
                } else {
                    0
                };
                if !is_ref && abs_frame_num > 0 {
                    abs_frame_num -= 1;
                }
                let mut expected = 0i32;
                if abs_frame_num > 0 {
                    let len = cycle.len() as u32;
                    let cycle_cnt = (abs_frame_num - 1) / len;
                    let in_cycle = (abs_frame_num - 1) % len;
                    let delta_per_cycle: i32 = cycle.iter().sum();
                    expected = cycle_cnt as i32 * delta_per_cycle
                        + cycle[..=in_cycle as usize].iter().sum::<i32>();
                }
                if !is_ref {
                    expected += offset_for_non_ref_pic;
                }
                expected + sh.delta_pic_order_cnt[0]
            }
            PocType::Type2 => {
                let max_frame_num = 1u32 << sps.log2_max_frame_num;
                let frame_num_offset = if is_idr {
                    0
                } else if self.prev_frame_num > sh.frame_num {
                    self.prev_frame_num_offset + max_frame_num
                } else {
                    self.prev_frame_num_offset
                };
                self.prev_frame_num_offset = frame_num_offset;
                self.prev_frame_num = sh.frame_num;
                let tmp = 2 * (frame_num_offset + sh.frame_num) as i32;
                if is_ref {
                    tmp
                } else {
                    tmp - 1
                }
            }
        }
    }

    /// MMCO 5 resets POC anchors (8.2.1, "memory_management_control_operation
    /// equal to 5").
    pub fn mmco5(&mut self, current_poc_after_reset: i32) {
        self.prev_pic_order_cnt_msb = 0;
        self.prev_pic_order_cnt_lsb = current_poc_after_reset;
        self.prev_frame_num = 0;
        self.prev_frame_num_offset = 0;
    }
}
