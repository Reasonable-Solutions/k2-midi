use claxon::metadata;
use claxon::FlacReader;
use crossbeam::channel::{bounded, Receiver, Sender};
use eframe::egui;
use egui::*;
use jack::{AudioOut, Client, ClientOptions, Control, ProcessScope};
use nats;
use rtrb::RingBuffer;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;
use std::{fs, thread};

pub static IS_PLAYING: AtomicBool = AtomicBool::new(true);
pub static CURRENT_POSITION: AtomicU32 = AtomicU32::new(0);

#[derive(Debug)]
enum PlayerCommand {
    ChangeSong(PathBuf),
}
#[derive(Debug)]
enum MetaCommand {
    Metadata(String, String),
}

struct PlayerApp {
    current_title: String,
    current_artist: String,
    meta_rx: Receiver<MetaCommand>,
}

impl PlayerApp {
    fn new(cc: &eframe::CreationContext, meta_rx: Receiver<MetaCommand>) -> Self {
        Self {
            current_title: String::from("Unknown"),
            current_artist: String::from("Unknown"),
            meta_rx,
        }
    }
}

impl eframe::App for PlayerApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Check for metadata updates
        if let Ok(cmd) = self.meta_rx.try_recv() {
            match cmd {
                MetaCommand::Metadata(title, artist) => {
                    self.current_title = title;
                    self.current_artist = artist;
                }
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label(CURRENT_POSITION.load(Ordering::Relaxed).to_string());
            ui.label("TRACK:");
            ui.label(&self.current_title);
            ui.label(&self.current_artist);
        });
        ctx.request_repaint_after(Duration::from_millis(12));
    }
}
fn main() {
    let (client, _status) = Client::new("flac_player", ClientOptions::NO_START_SERVER)
        .expect("Failed to create JACK client");
    let jack_buffer_size = client.buffer_size();
    let jack_sample_rate = client.sample_rate();
    println!(
        "JACK buffer size: {}, sample rate: {}",
        jack_buffer_size, jack_sample_rate
    );

    let rtrb_buffer_size = jack_buffer_size * 32;
    let (mut producer, mut consumer) = RingBuffer::<(f32, f32)>::new(rtrb_buffer_size as usize);

    let (cmd_tx, cmd_rx) = bounded::<PlayerCommand>(32);
    let (meta_tx, meta_rx) = bounded::<MetaCommand>(32);

    let mut out_port_left = client
        .register_port("out_left", AudioOut::default())
        .expect("Failed to create left output port");
    let mut out_port_right = client
        .register_port("out_right", AudioOut::default())
        .expect("Failed to create right output port");

    let mut audio_file_path = PathBuf::from("./music/psy.flac");

    prefill_buffer(&audio_file_path, &mut producer, rtrb_buffer_size as usize);

    thread::spawn(move || {
        decode_flac(
            &mut audio_file_path,
            &mut producer,
            jack_sample_rate as u32,
            &cmd_rx,
            &meta_tx,
            rtrb_buffer_size as usize,
        );
    });

    let cmd_tx_clone = cmd_tx.clone();
    thread::spawn(move || {
        control_thread(cmd_tx_clone);
    });

    let process_callback = move |_: &Client, ps: &ProcessScope| -> Control {
        let out_buffer_left = out_port_left.as_mut_slice(ps);
        let out_buffer_right = out_port_right.as_mut_slice(ps);

        if !IS_PLAYING.load(Ordering::Relaxed) {
            // If not playing, output silence
            for (left, right) in out_buffer_left.iter_mut().zip(out_buffer_right.iter_mut()) {
                *left = 0.0;
                *right = 0.0;
            }
            return Control::Continue;
        }

        for (left, right) in out_buffer_left.iter_mut().zip(out_buffer_right.iter_mut()) {
            if let Ok((l, r)) = consumer.pop() {
                *left = l;
                *right = r;
            } else {
                *left = 0.0;
                *right = 0.0;
            }
        }
        Control::Continue
    };

    let active_client = client
        .activate_async((), jack::ClosureProcessHandler::new(process_callback))
        .expect("Failed to activate client");

    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "ANAHATA",
        native_options,
        Box::new(|cc| Ok(Box::new(PlayerApp::new(cc, meta_rx)))),
    );

    active_client
        .deactivate()
        .expect("Failed to deactivate client");
}

fn prefill_buffer(path: &PathBuf, producer: &mut rtrb::Producer<(f32, f32)>, buffer_size: usize) {
    let file = File::open(path).expect("Failed to open FLAC file");
    let mut reader = FlacReader::new(file).expect("Failed to create FLAC reader");
    let bits_per_sample = reader.streaminfo().bits_per_sample;
    let scale_factor = 1.0 / (1_i32 << (bits_per_sample - 1)) as f64;

    let mut left_sample = None;
    let mut samples_written = 0;

    for sample in reader.samples() {
        if samples_written >= buffer_size {
            break;
        }

        let sample = sample.expect("Failed to read sample") as f64 * scale_factor;
        let sample = sample.max(-1.0).min(1.0) as f32;

        match left_sample.take() {
            None => {
                left_sample = Some(sample);
            }
            Some(left) => {
                if producer.push((left, sample)).is_ok() {
                    samples_written += 1;
                }
            }
        }
    }
    println!("Buffer prefilled with {} samples", samples_written);
}

fn decode_flac(
    path: &mut PathBuf,
    producer: &mut rtrb::Producer<(f32, f32)>,
    output_sample_rate: u32,
    cmd_rx: &Receiver<PlayerCommand>,
    meta_tx: &Sender<MetaCommand>,
    skip_samples: usize,
) {
    let mut sample_index = skip_samples * 2;
    loop {
        if let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                PlayerCommand::ChangeSong(new_path) => {
                    *path = new_path;
                    sample_index = 0;
                    continue;
                }
            }
        }

        let file = File::open(&path).expect("Failed to open FLAC file");
        let mut reader = FlacReader::new(file).expect("Failed to create FLAC reader");
        let channels = reader.streaminfo().channels as usize;
        let bits_per_sample = reader.streaminfo().bits_per_sample;
        let sample_rate = reader.streaminfo().sample_rate;
        if channels != 2 {
            panic!("stereo please");
        }
        let title = reader.get_tag("title").next();
        let artist = reader.get_tag("artist").next();
        let album = reader.get_tag("album").next();

        meta_tx
            .send(MetaCommand::Metadata(
                title.unwrap_or("EH").to_owned(),
                artist.unwrap_or("AH").to_owned(),
            ))
            .expect("Failed to send metadata");

        let scale_factor = 1.0 / (1_i32 << (bits_per_sample - 1)) as f64;
        let mut samples = reader.samples().skip(sample_index);
        let mut left_sample = None;

        for sample in samples {
            while !IS_PLAYING.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_micros(100));
            }
            if sample_index % (sample_rate as usize * 2) == 0 {
                CURRENT_POSITION.store(
                    sample_index as u32 / (2 * sample_rate as u32),
                    Ordering::Relaxed,
                );
            }
            let sample = sample.expect("Failed to read sample") as f64 * scale_factor;
            let sample = sample.max(-1.0).min(1.0) as f32;

            match left_sample.take() {
                None => {
                    left_sample = Some(sample);
                    sample_index += 1;
                }
                Some(left) => {
                    while producer.push((left, sample)).is_err() {
                        thread::sleep(Duration::from_micros(10));
                    }
                    sample_index += 1;
                }
            }
        }
    }
}

fn control_thread(cmd_tx: Sender<PlayerCommand>) {
    let nc = nats::connect("nats://localhost:4222").expect("Failed to connect to NATS");

    let sub = nc
        .subscribe("xone.>")
        .expect("Failed to subscribe to stop topic");

    println!("Control thread started, listening for NATS messages");

    for msg in sub.messages() {
        dbg!(&msg);
        match msg.subject.as_ref() {
            "xone.library.stop" => {
                if IS_PLAYING.load(Ordering::Relaxed) == true {
                    println!("Received resume command via NATS");
                    IS_PLAYING.store(false, Ordering::Relaxed)
                } else {
                    println!("Received stop command via NATS");
                    IS_PLAYING.store(true, Ordering::Relaxed)
                };
            }
            "xone.player.1.select" => {
                let content = String::from_utf8_lossy(&msg.data);
                let path = PathBuf::from(content.into_owned());
                cmd_tx
                    .send(PlayerCommand::ChangeSong(path))
                    .expect("Failed to send command");
            }
            _ => {}
        }
    }
}
