#![deny(warnings)]
use pocopine::prelude::*;
struct State { source: u32, f0: u32, f1: u32, f2: u32, f3: u32, f4: u32, f5: u32, f6: u32, f7: u32, f8: u32, f9: u32, f10: u32, f11: u32, f12: u32, f13: u32, f14: u32, f15: u32, f16: u32, f17: u32, f18: u32, f19: u32, f20: u32, f21: u32, f22: u32, f23: u32, f24: u32, f25: u32, f26: u32, f27: u32, f28: u32, f29: u32, f30: u32, f31: u32, f32: u32 }
#[handlers]
impl State {
    #[watch(source)]
    fn large(source: u32) -> Update<Self, (Self::F0, Self::F1, Self::F2, Self::F3, Self::F4, Self::F5, Self::F6, Self::F7, Self::F8, Self::F9, Self::F10, Self::F11, Self::F12, Self::F13, Self::F14, Self::F15, Self::F16, Self::F17, Self::F18, Self::F19, Self::F20, Self::F21, Self::F22, Self::F23, Self::F24, Self::F25, Self::F26, Self::F27, Self::F28, Self::F29, Self::F30, Self::F31, Self::F32)> {
        Update::new().f0(source).f32(source)
    }
}
fn main() {}
