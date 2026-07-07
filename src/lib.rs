#![feature(unboxed_closures)]
#![feature(fn_traits)]
#![feature(portable_simd)]
pub mod player;
pub mod sampler;
pub mod resampler;
pub const NUMBER_COUNT: u8 = 8;
pub const LETTER_COUNT: u8 = 8;
pub const MANUAL_COUNT: usize = 3;
pub const REGISTER_COUNT: usize = 16*8;
pub const NOTE_COUNT: usize = 61;
pub const NOTE_START: usize = 36;
pub const PRELOAD_SAMPLES: usize = 10000;
