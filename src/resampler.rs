use rayon::prelude::*;
use std::f64::consts::PI;

use crate::player::SampleInfo;

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

fn kaiser(x: f64, taps: usize, beta: f64) -> f64 {
    let n = taps as f64;

    let r = 2.0 * x / n;

    if r.abs() > 1.0 {
        return 0.0;
    }

    let a = (1.0 - r * r).sqrt();

    bessel_i0(beta * a) / bessel_i0(beta)
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut y = 1.0;

    for k in 1..35 {
        y *= (x * x) / (4.0 * (k as f64).powi(2));
        sum += y;
    }

    sum
}

fn interpolate(samples: &[f32], pos: f64, loop_start: usize, loop_end: usize) -> [f32; 2] {
    const BETA: f64 = 9.0;
    const TAPS: usize = 256;

    let mut out = [0.0f64; 2];

    let center = pos.floor() as isize;
    let frac = pos - center as f64;
    let half = TAPS as isize / 2;

    for tap in -half..half {
        let mut index = center + tap;
        // loop-aware wrapping
        if index >= loop_start as isize {
            let len = (loop_end - loop_start) as isize;
            while index >= loop_end as isize {
                index -= len;
            }
        } else {
            // attack section
            if index < 0 {
                continue;
            }
        }

        let x = tap as f64 - frac;

        let w = sinc(x) * kaiser(x, TAPS, BETA);

        let frame = index as usize;

        for ch in 0..2 {
            out[ch] += samples[frame * 2 + ch] as f64 * w;
        }
    }
    [out[0] as f32, out[1] as f32]
}
pub fn resample(input: SampleInfo, speed: f64) -> SampleInfo {
    let loop_duration = input.loop_end - input.loop_start;
    let loop_duration_out_theoretical = loop_duration as f64 / speed;
    let speed_adjusted = loop_duration as f64 / loop_duration_out_theoretical.round();
    let loop_start_pos_float = input.loop_start as f64 / speed_adjusted;
    let loop_start = loop_start_pos_float.ceil() as usize;
    let loop_end = loop_start + loop_duration_out_theoretical.round() as usize;
    let start_offset = loop_start_pos_float - loop_start as f64; //negative
    let samples: Vec<_> = (0..=loop_end)
        .into_par_iter()
        .flat_map(|int_idx| {
            interpolate(
                &input.samples,
                int_idx as f64 * speed_adjusted + start_offset,
                input.loop_start,
                input.loop_end,
            )
        })
        .collect();
    SampleInfo {
        samples,
        loop_start,
        loop_end,
        speed,
    }
}
