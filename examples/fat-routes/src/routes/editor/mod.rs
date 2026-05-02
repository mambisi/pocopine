use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::shared::{route_seed, summarize};

static EDITOR_PAYLOAD: [u8; 32_768] = [0x57; 32_768];

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Editor {
    pub summary: String,
}

#[handlers]
impl Editor {
    pub fn on_mount(&mut self) {
        self.summary = summarize("editor", editor_score(route_seed("editor")));
    }
}

macro_rules! step {
    ($name:ident, $shift:literal, $mul:literal, $xor:literal) => {
        #[inline(never)]
        fn $name(value: u32) -> u32 {
            let value = std::hint::black_box(value);
            let mixed = value.wrapping_add($xor).rotate_right($shift) ^ $mul;
            mixed
                .wrapping_mul(33)
                .wrapping_add(value.rotate_left($shift + 1))
        }
    };
}

step!(editor_00, 2, 0x9e37_79b9, 0x243f_6a88);
step!(editor_01, 4, 0x85eb_ca6b, 0x85a3_08d3);
step!(editor_02, 6, 0xc2b2_ae35, 0x1319_8a2e);
step!(editor_03, 8, 0x27d4_eb2f, 0x0370_7344);
step!(editor_04, 10, 0x1656_67b1, 0xa409_3822);
step!(editor_05, 12, 0xd3a2_64bc, 0x299f_31d0);
step!(editor_06, 14, 0xfd70_46c5, 0x082e_fa98);
step!(editor_07, 16, 0xb55a_4f09, 0xec4e_6c89);
step!(editor_08, 18, 0xc761_c23c, 0x4528_21e6);
step!(editor_09, 20, 0x9a63_2d2b, 0x38d0_1377);
step!(editor_10, 22, 0xe703_7ed1, 0xbe54_66cf);
step!(editor_11, 24, 0x8eb4_4a87, 0x34e9_0c6c);
step!(editor_12, 26, 0x5899_65cd, 0xc0ac_29b7);
step!(editor_13, 28, 0x1b87_3593, 0xc97c_50dd);
step!(editor_14, 30, 0x6c8e_9cf5, 0x3f84_d5b5);
step!(editor_15, 31, 0x4cf5_ad43, 0xb547_0917);

#[inline(never)]
fn editor_score(mut value: u32) -> u32 {
    value = editor_payload_mix(value);
    value = editor_00(value);
    value = editor_01(value);
    value = editor_02(value);
    value = editor_03(value);
    value = editor_04(value);
    value = editor_05(value);
    value = editor_06(value);
    value = editor_07(value);
    value = editor_08(value);
    value = editor_09(value);
    value = editor_10(value);
    value = editor_11(value);
    value = editor_12(value);
    value = editor_13(value);
    value = editor_14(value);
    editor_15(value)
}

#[inline(never)]
fn editor_payload_mix(mut value: u32) -> u32 {
    for (idx, byte) in EDITOR_PAYLOAD.iter().enumerate().step_by(251) {
        let byte = unsafe { std::ptr::read_volatile(byte) } as u32;
        value = value.wrapping_mul(33).rotate_right((idx % 29) as u32 + 1)
            ^ byte.wrapping_add(idx as u32);
    }
    value
}
