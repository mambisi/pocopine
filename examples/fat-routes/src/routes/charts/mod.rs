use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::shared::{route_seed, summarize};

static CHARTS_PAYLOAD: [u8; 32_768] = [0x31; 32_768];

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Charts {
    pub summary: String,
}

#[handlers]
impl Charts {
    pub fn on_mount(&mut self) {
        self.summary = summarize("charts", charts_score(route_seed("charts")));
    }
}

macro_rules! step {
    ($name:ident, $shift:literal, $mul:literal, $xor:literal) => {
        #[inline(never)]
        fn $name(value: u32) -> u32 {
            let value = std::hint::black_box(value);
            let mixed = value.wrapping_mul($mul).rotate_left($shift) ^ $xor;
            mixed.wrapping_add((mixed >> 7) | value.rotate_right($shift + 1))
        }
    };
}

step!(charts_00, 1, 0x045d_9f3b, 0x51ed_270b);
step!(charts_01, 3, 0x119d_e1f3, 0x2c1b_3c6d);
step!(charts_02, 5, 0x344b_1f13, 0x78d4_2a11);
step!(charts_03, 7, 0x27d4_eb2d, 0x0f9a_9237);
step!(charts_04, 9, 0x1656_67b1, 0x6511_31af);
step!(charts_05, 11, 0x85eb_ca6b, 0xc2b2_ae35);
step!(charts_06, 13, 0xc2b2_ae35, 0x27d4_eb2f);
step!(charts_07, 15, 0x27d4_eb2f, 0x1656_67b1);
step!(charts_08, 17, 0x9e37_79b1, 0x045d_9f3b);
step!(charts_09, 19, 0x7f4a_7c15, 0x119d_e1f3);
step!(charts_10, 21, 0x94d0_49bb, 0x344b_1f13);
step!(charts_11, 23, 0x2545_f491, 0x85eb_ca6b);
step!(charts_12, 25, 0x369d_eb2d, 0x51ed_270b);
step!(charts_13, 27, 0x6c8e_9cf5, 0x78d4_2a11);
step!(charts_14, 29, 0x4cf5_ad43, 0x0f9a_9237);
step!(charts_15, 31, 0x58f3_8ded, 0x6511_31af);

#[inline(never)]
fn charts_score(mut value: u32) -> u32 {
    value = charts_payload_mix(value);
    value = charts_00(value);
    value = charts_01(value);
    value = charts_02(value);
    value = charts_03(value);
    value = charts_04(value);
    value = charts_05(value);
    value = charts_06(value);
    value = charts_07(value);
    value = charts_08(value);
    value = charts_09(value);
    value = charts_10(value);
    value = charts_11(value);
    value = charts_12(value);
    value = charts_13(value);
    value = charts_14(value);
    charts_15(value)
}

#[inline(never)]
fn charts_payload_mix(mut value: u32) -> u32 {
    for (idx, byte) in CHARTS_PAYLOAD.iter().enumerate().step_by(257) {
        let byte = unsafe { std::ptr::read_volatile(byte) } as u32;
        value = value.rotate_left((idx % 31) as u32 + 1) ^ byte.wrapping_mul(idx as u32 + 1);
    }
    value
}
