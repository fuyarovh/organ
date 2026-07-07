use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, Write};
use std::mem;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use crate::resampler::resample;
use crate::sampler::{SampleMessage, Sampler};
use crate::{MANUAL_COUNT, NOTE_COUNT, NOTE_START, REGISTER_COUNT};
use bitmaps::Bitmap;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{HostId, StreamConfig, host_from_id};
use dasp_sample::I24;
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use riff::Chunk;
use scheduled_channel::{Receiver, Sender};
#[derive(Default, Clone, Debug)]
pub struct SampleInfo {
    pub samples: Vec<f32>,
    pub loop_start: usize,
    pub loop_end: usize,
    pub speed: f64,
}
pub struct Player {
    sinks: Vec<[Bitmap<{ crate::MANUAL_COUNT + 1 }>; NOTE_COUNT]>,
    notes: [Bitmap<NOTE_COUNT>; MANUAL_COUNT + 1],
    sample_info: Vec<[Option<SampleInfo>; NOTE_COUNT]>,
    registers: [Bitmap<{ REGISTER_COUNT }>; MANUAL_COUNT + 1],
    sender: Sender<ScheduledTask>,
    receiver: Receiver<ScheduledTask>,
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

impl Player {
    pub fn sender(&self) -> Sender<ScheduledTask> {
        self.sender.clone()
    }
    pub fn new(path: &str) -> Self {
        let mut sample_info = vec![const { [const { None }; NOTE_COUNT] }; REGISTER_COUNT];
        let mut order =
            BufReader::new(File::open(format!("{path}/order")).expect("couldn't open order file"))
                .lines()
                .filter(|x| x.as_ref().is_ok_and(|y| !y.is_empty()))
                .enumerate()
                .filter(|(_, x)| x.as_ref().is_ok_and(|y| y != "-"));
        while let Some((reg_idx, Ok(register))) = order.next() {
            let reg_path = format!("{path}/{register}");
            if !Path::new(&reg_path).exists() {
                break;
            }
            let volume_multiplier = fs::read_to_string(format!("{reg_path}/VOLUME"))
                .unwrap_or("1.0".to_owned())
                .parse()
                .unwrap_or(1.0);
            println!("found reg {register}");
            for note in 0..NOTE_COUNT {
                let actual_note = note + NOTE_START;
                let file_name = format!("{reg_path}/{actual_note}.wav");
                if let Ok(mut file) = OpenOptions::new().read(true).write(true).open(&file_name)
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
                    let _sample_period_nano = u32::from_le_bytes(contents.pop().unwrap());
                    let smpl_note = u32::from_le_bytes(contents.pop().unwrap());
                    let semitone_distance =
                        (smpl_note as i32 - actual_note as i32 + 6).rem_euclid(12) - 6;
                    let fine_tune = u32::from_le_bytes(contents.pop().unwrap()) as f64
                        / (u32::MAX as f64 + 1.0);
                    // let pitch_fraction = (u32::from_le_bytes(contents.pop().unwrap()) as f64
                    //     / (u32::MAX as f64 + 1.0))
                    //     + (((smpl_note as i32 - actual_note as i32) % 12 + 12) % 12) as f64;
                    let mut speed = 1.0 / 2f64.powf((semitone_distance as f64 + fine_tune) / 12f64);
                    if (1.0 - speed).abs() < 0.0022 {
                        speed = 1.0;
                    }
                    let _smpt_format = contents.pop();
                    let _smpt_offset = contents.pop();
                    let _num_loops = contents.pop();
                    let _sample_data = contents.pop();
                    let _cue_point = contents.pop();
                    let _type = contents.pop();
                    let loop_start = u32::from_le_bytes(contents.pop().unwrap());
                    let loop_end = u32::from_le_bytes(contents.pop().unwrap());
                    // let loop_duration = Duration::from_secs_f32(
                    //     (loop_end - loop_start) as f32 * sample_period_nano as f32 * 1000000000f32 /speed as f32,
                    // );
                    let mut wav_reader = WavReader::open(&file_name).unwrap();
                    let _channels = wav_reader.spec().channels;
                    // let _sample_rate = (wav_reader.spec().sample_rate as f32 * speed) as u32;
                    let samples: Vec<f32> = if wav_reader.spec().sample_format == SampleFormat::Float {
                        wav_reader.samples::<f32>().flatten().collect()
                    } else {
                        wav_reader.samples::<i32>().flatten().map(|x| {
                            volume_multiplier
                                * dasp_sample::conv::i24::to_f32(I24::new_unchecked(x))
                        })
                        // .take(PRELOAD_SAMPLES)
                        .collect()
                    };

                    let mut info = SampleInfo {
                        loop_start: loop_start as usize,
                        loop_end: 1 + loop_end as usize, // exclusive to inclusive
                        speed,
                        samples,
                    };
                    if speed != 1.0 {
                        drop(file);
                        info = resample(info, speed);
                        {
                            let write_spec = WavSpec {
                                channels: 2,
                                sample_rate: 48000,
                                bits_per_sample: 32,
                                sample_format: hound::SampleFormat::Float,
                            };
                            let mut wav_writer = WavWriter::create(&file_name, write_spec).unwrap();
                            for &sample in &info.samples {
                                wav_writer.write_sample(sample).unwrap();
                            }
                        }
                        if let Ok(mut file) = OpenOptions::new().write(true).open(&file_name) {
                            let smpl_bytes: Vec<_> = b"smpl"
                                .to_owned()
                                .into_iter()
                                .chain(
                                    [
                                        60, //size
                                        0,  //manufacturer
                                        0,  //product
                                        0,  //sample_period_nano TODO maybe calculate
                                        actual_note as u32,
                                        0, //pitch fraction
                                        0, //smpt_format
                                        0, //smpt_offset
                                        1, //num_loops
                                        0, //size of sample_data after loop
                                        0, //loop cue id
                                        0, //loop type
                                        info.loop_start as u32,
                                        info.loop_end as u32 - 1, //inclusive to exclusive
                                        0,                        //loop fracton
                                        0,                        //loop play count
                                    ]
                                    .into_iter()
                                    .flat_map(|x: u32| x.to_le_bytes()),
                                )
                                .collect();
                            file.seek(std::io::SeekFrom::End(0)).unwrap();
                            file.write_all(&smpl_bytes).unwrap();
                            let file_len = file.seek(std::io::SeekFrom::End(0)).unwrap();
                            let riff_size = (file_len - 8) as u32;
                            file.seek(std::io::SeekFrom::Start(4)).unwrap();
                            file.write_all(&riff_size.to_le_bytes()).unwrap();
                        }
                    }
                    info.samples.truncate(info.loop_end as usize * 2 + 2);
                    let mut start_samples =
                        info.samples[2 * info.loop_start..=2 * info.loop_start + 32].to_vec();
                    info.samples.append(&mut start_samples);
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
        println!("finished loading samples");

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

        //these loops are there because for some reason these functions may fail the first time
        let host = loop {
            match host_from_id(HostId::Jack) {
                Ok(host) => break host,
                Err(e) => println!("{e:?}"),
            }
            sleep(Duration::from_secs(1));
        };
        let device = loop {
            match host.default_output_device() {
                Some(device) => break device,
                None => println!("No device found"),
            }
            sleep(Duration::from_secs(1));
        };
        let (sampler, stream_sender) = Sampler::new(mem::take(&mut self.sample_info));
        let config = StreamConfig {
            channels: 2,
            sample_rate: 48000,
            buffer_size: cpal::BufferSize::Fixed(64),
        };
        let stream = device
            .build_output_stream(
                config,
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
