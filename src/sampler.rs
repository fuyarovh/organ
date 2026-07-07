use std::{
    simd::Simd,
    sync::mpsc::{Receiver, SyncSender, sync_channel},
};

use cpal::OutputCallbackInfo;

use crate::{NOTE_COUNT, REGISTER_COUNT, player::SampleInfo};

const FRAME_COUNT: usize = 4;
// #[derive(Default)]
#[derive(Clone)]
struct Samples {
    pub sample_info: &'static SampleInfo,
    pub index: usize,
    pub fade_out_multiplier: f32,
    pub fade_out: bool,
}

impl Samples {
    pub fn new(sample_info: &'static SampleInfo) -> Self {
        Self {
            fade_out_multiplier: 1f32,
            sample_info,
            index: 0,
            fade_out: Default::default(),
        }
    }
}
const fn const_powi(base: f32, exp: u32) -> f32 {
    let mut i = exp;
    let mut res = 1.0;
    while i > 0 {
        res *= base;
        i -= 1;
    }
    res
}
impl Iterator for Samples {
    type Item = Simd<f32, { 2 * FRAME_COUNT }>;

    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> {
        const FADE_BASE: f32 = 0.998;
        if self.fade_out_multiplier <= 0.00002 {
            return None;
        }
        let index = self.index;
        self.index += FRAME_COUNT;
        if self.index >= self.sample_info.loop_end {
            self.index -= self.sample_info.loop_end - self.sample_info.loop_start;
        }

        let ret = Simd::from_slice(&self.sample_info.samples[index * 2..]);
        if self.fade_out {
            let ret =
                ret * Simd::from_array(
                    const {
                        let mut array = [1.0; 2 * FRAME_COUNT];
                        let mut i = 2;
                        while i < FRAME_COUNT * 2 {
                            let last_val = array[i - 2];
                            array[i] = last_val * FADE_BASE;
                            i += 1;
                        }
                        array
                    },
                ) * Simd::splat(self.fade_out_multiplier);
            self.fade_out_multiplier *= const { const_powi(FADE_BASE, FRAME_COUNT as u32) };
            Some(ret)
        } else {
            Some(ret)
        }
    }
}

pub enum SampleMessage {
    NewNote((u8, u8)),
    Stop(u8, u8),
}
pub struct Sampler {
    // sample_map: HashMap<((u8, u8), u8), Samples>,
    sample_info: &'static [[Option<SampleInfo>; NOTE_COUNT]],
    receiver: Receiver<SampleMessage>,
    active_voices: Vec<[[Option<Samples>; 16]; REGISTER_COUNT]>,
    active_voice_indices: Vec<(usize, usize, usize)>,
    is_rt_configured: bool,
}

impl Sampler {
    pub fn new(
        sample_info: Vec<[Option<SampleInfo>; NOTE_COUNT]>,
    ) -> (Self, SyncSender<SampleMessage>) {
        let (sender, receiver) = sync_channel(2000);
        let sample_info = Box::new(sample_info).leak();
        (
            Self {
                receiver,
                sample_info,
                active_voices: vec![
                    const { [const { [const { None }; 16] }; REGISTER_COUNT] };
                    NOTE_COUNT
                ],
                active_voice_indices: Default::default(),
                is_rt_configured: false,
            },
            sender,
        )
    }
}
impl FnMut<(&mut [f32], &OutputCallbackInfo)> for Sampler {
    extern "rust-call" fn call_mut(
        &mut self,
        (data, _info): (&mut [f32], &OutputCallbackInfo),
    ) -> Self::Output {
        if !self.is_rt_configured {
            unsafe {
                libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE);
                let mut param: libc::sched_param = std::mem::zeroed();
                param.sched_priority = 85;
                libc::sched_setscheduler(0, libc::SCHED_FIFO, &param);
            }
            self.is_rt_configured = true;
        }
        while let Ok(message) = self.receiver.try_recv() {
            match message {
                SampleMessage::NewNote(key) => {
                    if let Some(sample_info) = &self.sample_info[key.0 as usize][key.1 as usize] {
                        if let Some(sample) =
                            self.active_voices[key.0 as usize][key.1 as usize][0].take()
                            && let Some(idx) = (1..16usize).find(|&x| {
                                self.active_voices[key.0 as usize][key.1 as usize][x].is_none()
                            })
                        {
                            self.active_voices[key.0 as usize][key.1 as usize][idx] = Some(sample);
                            self.active_voice_indices
                                .push((key.0 as usize, key.1 as usize, idx));
                        } else {
                            self.active_voice_indices
                                .push((key.0 as usize, key.1 as usize, 0));
                        }
                        self.active_voices[key.0 as usize][key.1 as usize][0] =
                            Some(Samples::new(sample_info));
                    }
                }
                SampleMessage::Stop(val0, val1) => {
                    if let Some(Some(value)) =
                        self.active_voices[val0 as usize][val1 as usize].get_mut(0)
                    {
                        value.fade_out = true;
                    }
                }
            }
        }
        data.fill(0.0);

        self.active_voice_indices.retain(|idx| {
            let voice_option = &mut self.active_voices[idx.0][idx.1][idx.2];
            if let Some(voice) = voice_option {
                let count = data
                    .as_chunks_mut::<{ FRAME_COUNT * 2 }>()
                    .0
                    .iter_mut()
                    .zip(voice)
                    .map(|(out, input)| {
                        (Simd::from_slice(out) + input).copy_to_slice(out);
                    })
                    .count();
                if count * FRAME_COUNT * 2 == data.len() {
                    return true;
                }
            }
            *voice_option = None;
            false
        });
        for chunk in data.as_chunks_mut::<{ FRAME_COUNT * 2 }>().0 {
            (Simd::<f32, { FRAME_COUNT * 2 }>::from_slice(chunk) * Simd::splat(0.25))
                .copy_to_slice(chunk);
        }
    }
}
impl FnOnce<(&mut [f32], &OutputCallbackInfo)> for Sampler {
    type Output = ();

    extern "rust-call" fn call_once(
        mut self,
        args: (&mut [f32], &OutputCallbackInfo),
    ) -> Self::Output {
        self.call_mut(args)
    }
}
