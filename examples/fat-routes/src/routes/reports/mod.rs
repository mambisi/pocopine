use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::shared::{route_seed, summarize};

static REPORTS_PAYLOAD: [u8; 32_768] = [0x73; 32_768];

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Reports {
    pub summary: String,
}

#[handlers]
impl Reports {
    pub fn on_mount(&mut self) {
        self.summary = summarize("reports", reports_score(route_seed("reports")));
    }
}

macro_rules! step {
    ($name:ident, $shift:literal, $mul:literal, $xor:literal) => {
        #[inline(never)]
        fn $name(value: u32) -> u32 {
            let value = std::hint::black_box(value);
            let mixed = value ^ value.rotate_left($shift);
            mixed
                .wrapping_mul($mul)
                .wrapping_sub(($xor as u32).rotate_right($shift))
        }
    };
}

step!(reports_00, 1, 0x2c1b_3c6d, 0x85eb_ca6b);
step!(reports_01, 3, 0x78d4_2a11, 0xc2b2_ae35);
step!(reports_02, 5, 0x0f9a_9237, 0x27d4_eb2f);
step!(reports_03, 7, 0x6511_31af, 0x1656_67b1);
step!(reports_04, 9, 0xc2b2_ae35, 0x9e37_79b9);
step!(reports_05, 11, 0x27d4_eb2f, 0x85a3_08d3);
step!(reports_06, 13, 0x1656_67b1, 0x1319_8a2e);
step!(reports_07, 15, 0x045d_9f3b, 0x0370_7344);
step!(reports_08, 17, 0x119d_e1f3, 0xa409_3822);
step!(reports_09, 19, 0x344b_1f13, 0x299f_31d0);
step!(reports_10, 21, 0x85eb_ca6b, 0x082e_fa98);
step!(reports_11, 23, 0x7f4a_7c15, 0xec4e_6c89);
step!(reports_12, 25, 0x94d0_49bb, 0x4528_21e6);
step!(reports_13, 27, 0x2545_f491, 0x38d0_1377);
step!(reports_14, 29, 0x369d_eb2d, 0xbe54_66cf);
step!(reports_15, 31, 0x6c8e_9cf5, 0x34e9_0c6c);

#[inline(never)]
fn reports_score(mut value: u32) -> u32 {
    value = reports_payload_mix(value);
    value = reports_00(value);
    value = reports_01(value);
    value = reports_02(value);
    value = reports_03(value);
    value = reports_04(value);
    value = reports_05(value);
    value = reports_06(value);
    value = reports_07(value);
    value = reports_08(value);
    value = reports_09(value);
    value = reports_10(value);
    value = reports_11(value);
    value = reports_12(value);
    value = reports_13(value);
    value = reports_14(value);
    reports_15(value)
}

#[inline(never)]
fn reports_payload_mix(mut value: u32) -> u32 {
    for (idx, byte) in REPORTS_PAYLOAD.iter().enumerate().step_by(263) {
        let byte = unsafe { std::ptr::read_volatile(byte) } as u32;
        value = value
            .wrapping_add(byte << (idx % 7))
            .rotate_left((idx % 23) as u32 + 1)
            ^ 0x9e37_79b9;
    }
    value
}
