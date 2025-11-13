#![feature(duration_constructors)]
#![feature(thread_sleep_until)]
#![feature(unboxed_closures)]
#![feature(fn_traits)]
#![feature(f16)]
pub mod player;
pub mod sampler;
pub const NUMBER_COUNT: u8 = 8;
pub const LETTER_COUNT: u8 = 8;
pub const MANUAL_COUNT: usize = 3;
pub const REGISTER_COUNT: usize = 16*8;
pub const NOTE_COUNT: usize = 61;
pub const NOTE_START: usize = 36;
pub const PRELOAD_SAMPLES: usize = 10000;
pub const CROSSFADE_SAMPLES: usize = 500;
