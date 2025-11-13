use std::{
    collections::HashMap,
    fs::{self, read_link},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{Sender, channel},
    },
    thread::{self, ThreadId, sleep},
    time::Duration,
};

use alsa::seq::{self, EvNote, PortCap, PortSubscribe, PortType};
use hidapi::HidApi;
use oneshot::TryRecvError;
use organ::{
    LETTER_COUNT, MANUAL_COUNT, NOTE_START, NUMBER_COUNT, REGISTER_COUNT,
    player::{Player, ScheduledTask, ScheduledTaskType},
};
use pipewire::{context::ContextBox, main_loop::MainLoopBox, types::ObjectType};
use serde::{Deserialize, Serialize};

enum Message {
    AddRegisterHid(ThreadId, u8, Sender<[u8; 32]>),
    CloseHid(ThreadId, String),
    ToggleRegister(u8, u8),
    UpdateLEDs,
    AdvanceBank(i8, bool),
    ClearRegisters,
    ChangeBank(u8, u8),
    StartCalibration,
    SetBank(u8, u8),
    NewPwConnection(i32, PathBuf),
    ConnectManuals(Vec<i32>),
    SaveSettings,
}

// #[derive(Debug)]
// struct JackMessage {
//     note: u8,
//     pressed: bool,
//     channel: u8,
// }

#[derive(Serialize, Deserialize)]
struct Settings {
    pub all_stops: Vec<StopStatus>,
    pub hw_order: Option<Vec<PathBuf>>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            all_stops: vec![Default::default(); LETTER_COUNT as usize * NUMBER_COUNT as usize],
            hw_order: Default::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StopStatus(Vec<[bool; 4]>);

impl Default for StopStatus {
    fn default() -> Self {
        Self(vec![[false; 4]; REGISTER_COUNT])
    }
}

// fn reg_to_note(n: u8) -> u8 {
//     if n >= NOTE_START as u8 {
//         n + NOTE_COUNT as u8
//     } else {
//         n
//     }
// }

fn main() {
    let (message_sender, receiver) = channel();
    //pipewire

    let mut hidapi = HidApi::new().unwrap();
    hidapi.reset_devices().unwrap();

    let (sender_sender, sender_receiver) = oneshot::channel();
    thread::spawn(move || {
        let mut player = Player::new("./registers");
        sender_sender.send(player.sender()).unwrap();
        player.start();
    });
    let player_sender = sender_receiver.recv().unwrap();

    let device_paths = Arc::new(Mutex::new(Vec::<String>::new()));
    {
        let device_paths = device_paths.clone();
        let message_sender = message_sender.clone();
        thread::spawn(move || {
            let mut current_stops = StopStatus::default();
            let mut settings: Settings = fs::read_to_string("./settings")
                .ok()
                .and_then(|x| toml::from_str(&x).ok())
                .unwrap_or_default();
            let mut reg_hid_senders = Vec::new();
            let mut current_number = 0u8;
            let mut current_letter = 0u8;
            let mut alsa_thread_stop_sender = None;
            let mut port_to_hw: HashMap<i32, PathBuf> = HashMap::new();
            let mut hw_to_port = HashMap::new();
            'outer: loop {
                match receiver.recv().unwrap() {
                    Message::CloseHid(tid, path) => {
                        device_paths
                            .lock()
                            .unwrap()
                            .retain(|element| element != &path);
                        reg_hid_senders
                            .retain(|sender: &(ThreadId, u8, Sender<_>)| sender.0 != tid);
                    }
                    Message::ToggleRegister(register, manual) => {
                        let new_status = !current_stops.0[register as usize][manual as usize];
                        current_stops.0[register as usize][manual as usize] = new_status;
                        player_sender
                            .send(
                                ScheduledTask {
                                    note: 0,
                                    register,
                                    task_type: ScheduledTaskType::Register(manual, new_status),
                                },
                                None)
                            .unwrap();
                        message_sender.send(Message::UpdateLEDs).unwrap();
                    }
                    Message::UpdateLEDs => {
                        for (_, id, sender) in &reg_hid_senders {
                            let mut writebuf = [0; 32];
                            if *id < 16 {
                                let leds = current_stops.0.as_chunks::<8>().0[*id as usize];
                                writebuf[0] = id + 1;
                                for (col_idx, leds_col) in leds.iter().enumerate() {
                                    for (row_idx, led) in leds_col.iter().enumerate() {
                                        if *led {
                                            writebuf[row_idx + 1] |= 1 << col_idx;
                                        }
                                    }
                                }
                            } else {
                                writebuf[0] = 1;
                                writebuf[1] = current_letter;
                                writebuf[2] = current_number;
                            }
                            sender.send(writebuf).unwrap();
                        }
                    }
                    Message::AddRegisterHid(thread_id, id, sender) => {
                        reg_hid_senders.push((thread_id, id, sender));
                        println!("added device with id {id}");
                        message_sender.send(Message::UpdateLEDs).unwrap();
                    }
                    Message::AdvanceBank(offset, set) => {
                        let value = NUMBER_COUNT as i16 * LETTER_COUNT as i16
                            + (current_letter as i16 * NUMBER_COUNT as i16)
                            + current_number as i16
                            + offset as i16;
                        let new_letter = (value / NUMBER_COUNT as i16) % LETTER_COUNT as i16;
                        let new_number = value % NUMBER_COUNT as i16;

                        let message = if set {
                            Message::SetBank
                        } else {
                            Message::ChangeBank
                        };
                        message_sender
                            .send(message(new_letter as u8, new_number as u8))
                            .unwrap();
                    }
                    Message::ClearRegisters => {
                        current_stops = StopStatus::default();
                        message_sender.send(Message::UpdateLEDs).unwrap();
                    }
                    Message::ChangeBank(letter, number) => {
                        current_letter = letter;
                        current_number = number;
                        let last_stops = current_stops.clone();

                        current_stops = settings.all_stops
                            [letter as usize * NUMBER_COUNT as usize + number as usize]
                            .clone();
                        for (register, (&cur, &last)) in current_stops
                            .0
                            .iter()
                            .zip(last_stops.0.iter())
                            .enumerate()
                            .filter(|&(_idx, (cur, last))| cur != last)
                        {
                            for (manual, (&target, &_last)) in cur
                                .iter()
                                .zip(last.iter())
                                .enumerate()
                                .filter(|(_idx, (c, l))| c != l)
                            {
                                let register = register as u8;
                                player_sender
                                    .send(
                                        ScheduledTask {
                                            note: 0,
                                            register,
                                            task_type: ScheduledTaskType::Register(
                                                manual as u8,
                                                target,
                                            ),
                                        },
                                        None)
                                    .unwrap();
                            }
                        }
                        message_sender.send(Message::UpdateLEDs).unwrap();
                    }
                    Message::StartCalibration => {
                        println!("start calibration");
                        let seq =
                            alsa::Seq::open(None, Some(alsa::Direction::Capture), false).unwrap();
                        let port = seq
                            .create_simple_port(
                                c"Calibration",
                                PortCap::WRITE | PortCap::SUBS_WRITE,
                                PortType::MIDI_GENERIC,
                            )
                            .unwrap();
                        for &client in port_to_hw.keys() {
                            let subscribe_struct = PortSubscribe::empty().unwrap();
                            subscribe_struct.set_sender(seq::Addr { client, port: 0 });
                            subscribe_struct.set_dest(seq::Addr {
                                client: seq.client_id().unwrap(),
                                port,
                            });
                            if seq.subscribe_port(&subscribe_struct).is_err() {
                                continue 'outer;
                            }
                        }
                        let mut input = seq.input();
                        let mut order = Vec::new();
                        let mut hw_order = Vec::new();
                        let mut writebuf = [0; 32];
                        for i in 0..(MANUAL_COUNT + 2) as u8 {
                            println!("waiting for manual {i}");
                            writebuf[0] = 0xFE;
                            writebuf[1] = i.min(MANUAL_COUNT as u8);
                            for (_, _, sender) in &reg_hid_senders {
                                sender.send(writebuf).unwrap();
                            }
                            let client = loop {
                                if let Ok(event) = input.event_input()
                                    && event.get_type() == seq::EventType::Noteon
                                {
                                    break event.get_source().client;
                                }
                            };
                            order.push(client);
                            hw_order.push(port_to_hw.get(&client).unwrap().clone());
                        }
                        writebuf[1] = 0xFF;
                        for (_, _, sender) in &reg_hid_senders {
                            sender.send(writebuf).unwrap();
                        }
                        settings.hw_order = Some(hw_order);
                        message_sender.send(Message::ConnectManuals(order)).unwrap();
                        message_sender.send(Message::SaveSettings).unwrap();
                    }
                    Message::SetBank(letter, number) => {
                        settings.all_stops
                            [letter as usize * NUMBER_COUNT as usize + number as usize] =
                            current_stops.clone();
                        message_sender.send(Message::SaveSettings).unwrap();
                        message_sender
                            .send(Message::ChangeBank(letter, number))
                            .unwrap();
                    }
                    Message::SaveSettings => {
                        fs::write("./settings", toml::to_string(&settings).unwrap()).unwrap();
                    }
                    Message::NewPwConnection(client_id, persistent_name) => {
                        println!("new pipewire connection: {client_id}, {persistent_name:?}");
                        port_to_hw.insert(client_id, persistent_name.clone());
                        hw_to_port.insert(persistent_name, client_id);
                        if port_to_hw.len() == MANUAL_COUNT + 2
                            && let Some(hw_order) = &settings.hw_order
                        {
                            let order: Vec<_> = hw_order
                                .iter()
                                .filter_map(|x| hw_to_port.get(x))
                                .cloned()
                                .collect();
                            if order.len() == MANUAL_COUNT + 2 {
                                message_sender.send(Message::ConnectManuals(order)).unwrap();
                            }
                        }
                    }
                    Message::ConnectManuals(order) => {
                        println!("final order is {order:?}");
                        let (sender, stop_receiver) = oneshot::channel::<()>();
                        if let Some(old) = alsa_thread_stop_sender.replace(sender) {
                            old.send(()).unwrap_or_default();
                        }
                        //let jack_sender = jack_tx.clone();
                        let player_sender = player_sender.clone();
                        thread::spawn(move || {
                            let seq = alsa::Seq::open(None, Some(alsa::Direction::Capture), false)
                                .unwrap();
                            let port = seq
                                .create_simple_port(
                                    c"Manual in",
                                    PortCap::WRITE | PortCap::SUBS_WRITE,
                                    PortType::MIDI_GENERIC,
                                )
                                .unwrap();
                            let mut client_to_channel = HashMap::new();
                            for (channel, &client) in order.iter().enumerate() {
                                let subscribe_struct = PortSubscribe::empty().unwrap();
                                subscribe_struct.set_sender(seq::Addr { client, port: 0 });
                                subscribe_struct.set_dest(seq::Addr {
                                    client: seq.client_id().unwrap(),
                                    port,
                                });
                                seq.subscribe_port(&subscribe_struct).unwrap_or_default();
                                client_to_channel.insert(client, channel.min(MANUAL_COUNT) as u8);
                            }
                            let mut input = seq.input();
                            while stop_receiver.try_recv() == Err(TryRecvError::Empty) {
                                if let Ok(event) = input.event_input()
                                    && let event_type @ (seq::EventType::Noteon
                                    | seq::EventType::Noteoff) = event.get_type()
                                {
                                    let note_event: EvNote = event.get_data().unwrap();
                                    player_sender
                                        .send(
                                            ScheduledTask {
                                                note: note_event.note - NOTE_START as u8,
                                                register: 0,
                                                task_type: ScheduledTaskType::Note(
                                                    *client_to_channel
                                                        .get(&event.get_source().client)
                                                        .unwrap_or(&(MANUAL_COUNT as u8)),
                                                    event_type == seq::EventType::Noteon,
                                                ),
                                            },
                                            None)
                                        .unwrap();
                                }
                            }
                        });
                    }
                }
            }
        });
    }
    //pipewire stuff
    let pw_sender = message_sender.clone();
    thread::spawn(move || {
        let alsa_seq = alsa::Seq::open(None, None, false).unwrap();
        let mainloop = MainLoopBox::new(None).unwrap();
        let context = ContextBox::new(mainloop.loop_(), None).unwrap();
        let core = context.connect(None).unwrap();
        let registry = core.get_registry().unwrap();
        let _listener = registry
            .add_listener_local()
            .global(move |gobject| {
                if gobject.type_ == ObjectType::Port {
                    let props = gobject.props.unwrap();
                    if props.get("format.dsp") == Some("8 bit raw midi")
                        && props.get("port.direction") == Some("out")
                        && let Some(client_id) = props
                            .get("port.group")
                            .and_then(|p| p.split_once('_'))
                            .and_then(|(_, x)| x.parse::<i32>().ok())
                        && let Ok(card_id) = alsa_seq
                            .get_any_client_info(client_id)
                            .and_then(|x| x.get_card())
                        && let Ok(device_path) =
                            read_link(format!("/sys/class/sound/card{card_id}/device"))
                    {
                        pw_sender
                            .send(Message::NewPwConnection(client_id, device_path))
                            .unwrap();
                    }
                }
            })
            .register();
        mainloop.run();
    });
    loop {
        hidapi.add_devices(0xFEED, 0x06A0).unwrap();
        let new_devices: Vec<_> = hidapi
            .device_list()
            .filter(|device| device.usage_page() == 0xFF60 && device.usage() == 0x61)
            .cloned()
            .collect();
        let device_paths_cloned = device_paths.clone();
        for device_info in new_devices.iter().filter(|info| {
            !device_paths_cloned
                .lock()
                .unwrap()
                .contains(&info.path().to_str().unwrap().to_owned())
        }) {
            let sender = message_sender.clone();
            if let (Ok(device), path) = (
                device_info.open_device(&hidapi),
                device_info.path().to_str().unwrap().to_owned(),
            ) {
                device_paths
                    .lock()
                    .unwrap()
                    .push(device_info.path().to_str().unwrap().to_owned());
                thread::spawn(move || {
                    let mut registered = None;
                    let mut buf = [0; 32];
                    let (hid_writer_tx, hid_writer_rx) = channel::<[u8; 32]>();
                    let writebuf = [0xFFu8; 32];
                    device.write(&writebuf).unwrap();
                    device.set_blocking_mode(false).unwrap();
                    loop {
                        if let Ok(writebuf) = hid_writer_rx.try_recv() {
                            device.write(&writebuf).unwrap();
                        }
                        let read_result = device.read(&mut buf);
                        if let Ok(num_bytes) = read_result {
                            if num_bytes == 0 {
                                sleep(Duration::from_micros(2000));
                                continue;
                            }
                        } else {
                            break;
                        }
                        let message_type = buf[0];
                        let id = buf[1];
                        if registered.is_none() {
                            registered = Some(id);
                            sender
                                .send(Message::AddRegisterHid(
                                    thread::current().id(),
                                    id,
                                    hid_writer_tx.clone(),
                                ))
                                .unwrap();
                        }
                        match message_type {
                            1 => {
                                //change register
                                let manual = buf[2];
                                let register = buf[3];
                                sender
                                    .send(Message::ToggleRegister(register, manual))
                                    .unwrap();
                            }
                            2 => {
                                sender.send(Message::ClearRegisters).unwrap();
                            }
                            3 => {
                                //advance
                                let count = buf[2] as i8;
                                let set = buf[3] != 0;
                                sender.send(Message::AdvanceBank(count, set)).unwrap();
                            }
                            4 => {
                                let letter = buf[2];
                                let number = buf[3];
                                if number == NUMBER_COUNT {
                                    sender.send(Message::ClearRegisters).unwrap();
                                } else {
                                    sender.send(Message::ChangeBank(letter, number)).unwrap();
                                }
                            }
                            5 => {
                                sender.send(Message::StartCalibration).unwrap();
                            }
                            6 => {
                                let letter = buf[2];
                                let number = buf[3];
                                sender.send(Message::SetBank(letter, number)).unwrap();
                            }
                            _ => (),
                        }
                    }
                    println!("removed device");
                    sender
                        .send(Message::CloseHid(thread::current().id(), path))
                        .unwrap();
                });
            }
        }
        sleep(Duration::from_secs(5));
    }
}
