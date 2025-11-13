use std::fs::File;
use std::io::{BufRead, BufReader};
use std::mem;
use std::path::Path;

use crate::sampler::{SampleMessage, Sampler};
use crate::{MANUAL_COUNT, NOTE_COUNT, NOTE_START, REGISTER_COUNT};
use bitmaps::Bitmap;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{HostId, SampleRate, host_from_id};
use dasp_sample::I24;
use hound::WavReader;
use itertools::Itertools;
use riff::Chunk;
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters};
use scheduled_channel::{Receiver, Sender};
#[derive(Default, Clone, Debug)]
pub struct SampleInfo {
    pub pre: Vec<f32>,
    pub repeat: Vec<f32>,
    pub extra: Vec<f32>,
}
pub struct Player {
    sinks: Vec<[Bitmap<{ crate::MANUAL_COUNT + 1 }>; NOTE_COUNT]>,
    notes: [Bitmap<NOTE_COUNT>; MANUAL_COUNT + 1],
    sample_info: Vec<[Option<SampleInfo>; NOTE_COUNT]>,
    registers: [Bitmap<{ REGISTER_COUNT }>; MANUAL_COUNT + 1],
    sender: Sender<ScheduledTask>,
    receiver: Receiver<ScheduledTask>,
}
fn u8_to_u32(input: [u8; 4]) -> u32 {
    input[0] as u32
        | ((input[1] as u32) << 8)
        | ((input[2] as u32) << 16)
        | ((input[3] as u32) << 24)
}

#[derive(Default, Clone, Debug)]
pub enum ScheduledTaskType {
    // Renew(SamplesBuffer, Duration),
    Note(u8, bool),
    Register(u8, bool),
    Sample,
    FadeOut,
    #[default]
    None,
}
#[derive(Default, Clone, Debug)]
pub struct ScheduledTask {
    pub note: u8,
    pub register: u8,
    pub task_type: ScheduledTaskType,
}

fn resample(input: &[f32], ratio: f64) -> Vec<f32> {
    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: rubato::SincInterpolationType::Quadratic,
        oversampling_factor: 128,
        window: rubato::WindowFunction::BlackmanHarris2,
    };
    let mut deinterleaved = [const { Vec::new() }; 2];
    for (idx, &sample) in input.iter().enumerate() {
        deinterleaved[idx % 2].push(sample);
    }
    let mut resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, 1024, 2).unwrap();

    let mut out = (Vec::new(), Vec::new());
    let mut in_offset = 0;
    while in_offset < input.len() / 2 {
        let next = resampler.input_frames_next();
        let remaining = input.len() / 2 - in_offset;

        if remaining >= next {
            let chunk = vec![
                &deinterleaved[0][in_offset..in_offset + next],
                &deinterleaved[1][in_offset..in_offset + next],
            ];
            let mut out_chunk = resampler.process(chunk.as_slice(), None).unwrap();
            out.0.append(&mut out_chunk[0]);
            out.1.append(&mut out_chunk[1]);
            in_offset += next;
        } else {
            break;
        }
    }
    let chunk = vec![
        &deinterleaved[0][in_offset..],
        &deinterleaved[1][in_offset..],
    ];
    let mut out_chunk = resampler
        .process_partial((!chunk.is_empty()).then_some(chunk.as_slice()), None)
        .unwrap();
    out.0.append(&mut out_chunk[0]);
    out.1.append(&mut out_chunk[1]);

    out.0.into_iter().interleave(out.1).collect()
}

impl Player {
    pub fn sender(&self) -> Sender<ScheduledTask> {
        self.sender.clone()
    }
    pub fn new(path: &str) -> Self {
        let mut sample_info = vec![const { [const { None }; NOTE_COUNT] }; REGISTER_COUNT];
        let mut order = BufReader::new(File::open(format!("{path}/order")).expect("couldn't open order file")).lines().filter(|x|x.as_ref().is_ok_and(|y|!y.is_empty())).enumerate().filter(|(_,x)|x.as_ref().is_ok_and(|y|y != "-"));
        while let Some((reg_idx, Ok(register))) = order.next() {
            let reg_path = format!("{path}/{register}");
            if !Path::new(&reg_path).exists() {
                break;
            }
            println!("found reg {register}");
            for note in 0..NOTE_COUNT {
                let actual_note = note + NOTE_START;
                let file_name = format!("{reg_path}/{actual_note}.wav");
                if let Ok(mut file) = File::open(&file_name)
                    && let Some(smpl) = Chunk::read(&mut file, 0).ok().and_then(|root| {
                        root.iter(&mut file)
                            .flatten()
                            .find(|x| x.id().value == *b"smpl")
                    })
                {
                    let contents_u8 = smpl.read_contents(&mut file).unwrap();
                    let (contents_chunked, _remainder) = contents_u8.as_chunks::<4>();
                    let mut contents = Vec::from(contents_chunked);
                    contents.reverse();
                    let _manufacturer = contents.pop();
                    let _product = contents.pop();
                    let _sample_period_nano = u8_to_u32(contents.pop().unwrap());
                    let smpl_note = u8_to_u32(contents.pop().unwrap());
                    let pitch_fraction = (u8_to_u32(contents.pop().unwrap()) as f64
                        / (u32::MAX as f64 + 1.0))
                        + ((((smpl_note as i32 - actual_note as i32) % 12) + 12) % 12) as f64;
                    let speed = 2f64.powf(pitch_fraction / 12f64);
                    let _smpt_format = contents.pop();
                    let _smpt_offset = contents.pop();
                    let _num_loops = contents.pop();
                    let _sample_data = contents.pop();
                    let _cue_point = contents.pop();
                    let _type = contents.pop();
                    let mut loop_start = u8_to_u32(contents.pop().unwrap());
                    let mut loop_end = u8_to_u32(contents.pop().unwrap());
                    // let loop_duration = Duration::from_secs_f32(
                    //     (loop_end - loop_start) as f32 * sample_period_nano as f32 * 1000000000f32 /speed as f32,
                    // );
                    let mut wav_reader = WavReader::open(&file_name).unwrap();
                    let _channels = wav_reader.spec().channels;
                    // let _sample_rate = (wav_reader.spec().sample_rate as f32 * speed) as u32;
                    let mut samples: Vec<f32> = wav_reader
                        .samples::<i32>()
                        .flatten()
                        .map(|x| dasp_sample::conv::i24::to_f32(I24::new_unchecked(x)))
                        // .take(PRELOAD_SAMPLES)
                        .collect();

                    if (speed - 1.0).abs() < 0.0001 {
                        samples = resample(&samples, 1.0 / speed);
                        loop_start = (loop_start as f64 * speed).round() as u32;
                        loop_end = (loop_end as f64 * speed).round() as u32;
                    }
                    let pre = samples[0..(loop_start as usize * 2)].to_vec();
                    let repeat =
                        samples[(loop_start as usize * 2)..(loop_end as usize * 2)].to_vec();
                    let extra = samples[(loop_end as usize * 2)..].to_vec();
                    let info = SampleInfo { pre, repeat, extra };
                    sample_info[reg_idx][note] = Some(info);
                } else {
                    sample_info[reg_idx][note] = None;
                }
            }
        }
        let (sender, receiver) = scheduled_channel::bounded(10000);
        let mut sinks = Vec::new();
        sinks.resize_with(REGISTER_COUNT, || {
            [const { Bitmap::from_value(0) }; NOTE_COUNT]
        });

        Player {
            sinks,
            notes: [const { Bitmap::from_value(0) }; MANUAL_COUNT + 1],
            sample_info,
            registers: [const { Bitmap::from_value(0) }; MANUAL_COUNT + 1],
            sender,
            receiver,
        }
    }
    pub fn start(&mut self) {
        println!("entered start function");
        let host = host_from_id(HostId::Alsa).unwrap();
        let device = host.default_output_device().unwrap();
        let (sampler, stream_sender) = Sampler::new(mem::take(&mut self.sample_info));
        let mut supported_configs_range = device.supported_output_configs().unwrap();
        let supported_config = supported_configs_range
            .next()
            .unwrap()
            .with_sample_rate(SampleRate(48000));
        let mut config = supported_config.config();
        config.buffer_size = cpal::BufferSize::Fixed(128);

        let stream = device
            .build_output_stream(
                &config,
                sampler,
                move |err| {
                    println!("{err:?}");
                },
                None,
            )
            .unwrap();
        stream.play().unwrap();

        loop {
            match self.receiver.recv() {
                Ok(ScheduledTask {
                    note,
                    register,
                    task_type,
                }) => {
                    match task_type {
                        ScheduledTaskType::FadeOut => {
                            stream_sender
                                .send(SampleMessage::Stop(register, note))
                                .unwrap();
                        }
                        ScheduledTaskType::Note(manual, val) => {
                            self.notes[manual as usize].set(note as usize, val);
                            let sender = self.sender();
                            for (register, rank) in
                                self.sinks.iter_mut().enumerate().filter(|(idx, _val)| {
                                    self.registers[manual as usize].get(*idx) | !val
                                })
                            {
                                let register = register as u8;
                                let bitmap = &mut rank[note as usize];
                                let oldval = !bitmap.is_empty();
                                bitmap.set(manual as usize, val);
                                let newval = !bitmap.is_empty();
                                match (oldval, newval) {
                                    (true, false) => {
                                        sender
                                            .send(
                                                ScheduledTask {
                                                    task_type: ScheduledTaskType::FadeOut,
                                                    register,
                                                    note,
                                                },
                                                None,
                                            )
                                            .unwrap();
                                    }
                                    (false, true) => {
                                        sender
                                            .send(
                                                ScheduledTask {
                                                    task_type: ScheduledTaskType::Sample,
                                                    register,
                                                    note,
                                                },
                                                None,
                                            )
                                            .unwrap();
                                    }
                                    _ => (),
                                }
                            }
                        }
                        ScheduledTaskType::Register(manual, val) => {
                            self.registers[manual as usize].set(register as usize, val);
                            let sender = self.sender();
                            let rank = &mut self.sinks[register as usize];
                            for (note, bitmap) in rank
                                .iter_mut()
                                .enumerate()
                                .filter(|(idx, _)| self.notes[manual as usize].get(*idx) | !val)
                            {
                                let note = note as u8;
                                let oldval = !bitmap.is_empty();
                                bitmap.set(manual as usize, val);
                                let newval = !bitmap.is_empty();
                                match (oldval, newval) {
                                    (true, false) => {
                                        sender
                                            .send(
                                                ScheduledTask {
                                                    task_type: ScheduledTaskType::FadeOut,
                                                    register,
                                                    note,
                                                },
                                                None,
                                            )
                                            .unwrap();
                                    }
                                    (false, true) => {
                                        sender
                                            .send(
                                                ScheduledTask {
                                                    task_type: ScheduledTaskType::Sample,
                                                    register,
                                                    note,
                                                },
                                                None,
                                            )
                                            .unwrap();
                                    }
                                    _ => (),
                                }
                            }
                        }
                        ScheduledTaskType::Sample => {
                            stream_sender
                                .send(SampleMessage::NewNote(
                                    (register, note), // sample_info.preload_samples.clone(),
                                ))
                                .unwrap();
                        }
                        ScheduledTaskType::None => (),
                    }
                }
                Err(er) => {
                    println!("{er:?}");
                    break;
                }
            }
        }
    }
}
