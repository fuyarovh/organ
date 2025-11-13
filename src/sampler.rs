use std::{
    collections::HashMap,
    sync::mpsc::{Receiver, SyncSender, sync_channel},
};

use cpal::OutputCallbackInfo;

use crate::{CROSSFADE_SAMPLES, NOTE_COUNT, player::SampleInfo};

// #[derive(Default)]
struct Samples {
    pub pre: &'static [f32],
    pub repeat: &'static [f32],
    pub extra: &'static [f32],
    pub index: usize,
    pub fade_out_multiplier: f32,
    pub fade_out: bool,
    pub repeating: bool,
    pub has_repeated: bool,
}

impl Samples {
    pub fn new(pre: &'static [f32], repeat: &'static [f32], extra: &'static [f32]) -> Self {
        Self {
            pre,
            fade_out_multiplier: 1f32,
            repeat,
            extra,
            index: Default::default(),
            fade_out: Default::default(),
            repeating: Default::default(),
            has_repeated: Default::default(),
            // ..Default::default()
        }
    }
}

impl Iterator for Samples {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.fade_out {
            self.fade_out_multiplier *= 0.9998;
            if self.fade_out_multiplier < 0.005 {
                self.fade_out_multiplier *= 0.998;
            }
        }
        (self.fade_out_multiplier > 0.0001).then_some(
            self.fade_out_multiplier
                * if self.repeating {
                    let mut ret = *self.repeat.get(self.index).unwrap();
                    let crossfade_samples = CROSSFADE_SAMPLES.min(self.extra.len());
                    if self.index < crossfade_samples && self.has_repeated {
                        let crossfade_val = self.extra.get(self.index).cloned().unwrap_or(ret);
                        ret = ret * self.index as f32 / crossfade_samples as f32
                            + crossfade_val * (1f32 - self.index as f32 / crossfade_samples as f32);
                    }
                    self.index = if self.index + 1 == self.repeat.len() {
                        self.has_repeated = true;
                        0
                    } else {
                        self.index + 1
                    };
                    ret
                } else if let Some(val) = self.pre.get(self.index).cloned() {
                    self.index += 1;
                    val
                } else {
                    self.repeating = true;
                    // self.pre.clear();
                    self.index = 0;
                    self.next().unwrap_or_default()
                },
        )
    }
}

pub enum SampleMessage {
    NewNote((u8, u8)),
    Stop(u8, u8),
}
pub struct Sampler {
    sample_map: HashMap<((u8, u8), u8), Samples>,
    sample_info: &'static [[Option<SampleInfo>; NOTE_COUNT]],
    receiver: Receiver<SampleMessage>,
}

impl Sampler {
    pub fn new(
        sample_info: Vec<[Option<SampleInfo>; NOTE_COUNT]>,
    ) -> (Self, SyncSender<SampleMessage>) {
        let (sender, receiver) = sync_channel(2000);
        let sample_map = HashMap::new();
        let sample_info = Box::new(sample_info).leak();
        (
            Self {
                sample_map,
                receiver,
                sample_info,
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
        while let Ok(message) = self.receiver.try_recv() {
            match message {
                SampleMessage::NewNote(key) => {
                    if let Some(sample_info) = &self.sample_info[key.0 as usize][key.1 as usize] {
                        if let Some(sample) = self.sample_map.remove(&(key, 0))
                            && let Some(idx) = (1..16u8).find(|x| !self.sample_map.contains_key(&(key,*x))) {
                                self.sample_map.insert((key,idx), sample);
                            } 
                        self.sample_map
                            .insert((key,0), Samples::new(&sample_info.pre, &sample_info.repeat, &sample_info.extra));
                    }
                }
                SampleMessage::Stop(val0, val1) => {
                    let key = ((val0, val1),0);
                    if let Some(value) = self.sample_map.get_mut(&key) {
                        value.fade_out = true;
                    }
                }
            }
        }
        let mut garbage = Vec::new();
        for point in data.iter_mut() {
            *point = 0.0;
        }
        for (key, iterator) in self.sample_map.iter_mut() {
            let count = data
                .iter_mut()
                .zip(iterator)
                .map(|(out, input)| *out += 0.2 * input)
                .count();
            if count < data.len() {
                garbage.push(*key);
            }
        }
        for entry in garbage {
            self.sample_map.remove(&entry);
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
