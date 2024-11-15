use crossbeam::channel::{bounded, Receiver, Sender};
use eframe::egui;
use egui::*;
use jack::{AudioOut, Client, ClientOptions, Control, ProcessScope};
use memmap2::Mmap;
use nats;
use rtrb::RingBuffer;
use std::error::Error;
use std::fs::File;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;
use std::{fs, thread};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub static IS_PLAYING: AtomicBool = AtomicBool::new(true);
pub static CURRENT_POSITION: AtomicU32 = AtomicU32::new(0);
pub static DURATION: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
enum PlayerCommand {
    ChangeSong(PathBuf),
    SkipForward,
    SkipBackward,
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
            ui.heading(
                egui::RichText::new("ANAHATA")
                    .size(50.0)
                    .strong()
                    .color(egui::Color32::WHITE),
            );
            ui.heading(format!(
                "-{:02}:{:02}-",
                CURRENT_POSITION.load(Ordering::Relaxed) / 60,
                CURRENT_POSITION.load(Ordering::Relaxed) % 60,
            ));
            ui.heading(format!(
                "-{:02}:{:02}-",
                DURATION.load(Ordering::Relaxed) / 60,
                DURATION.load(Ordering::Relaxed) % 60,
            ));

            ui.heading("TRACK:");
            ui.heading(&self.current_title);
            ui.heading(&self.current_artist);
        });
        ctx.request_repaint_after(Duration::from_millis(12));
    }
}

fn main() {
    let (client, _status) = Client::new("ANAHATA", ClientOptions::NO_START_SERVER)
        .expect("Failed to create JACK client");
    let jack_buffer_size = client.buffer_size();
    let jack_sample_rate = client.sample_rate();
    println!(
        "JACK buffer size: {}, sample rate: {}",
        jack_buffer_size, jack_sample_rate
    );

    let rtrb_buffer_size = jack_buffer_size * 2;
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

    thread::spawn(move || {
        decode_flac(
            &mut audio_file_path,
            &mut producer,
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

fn decode_flac(
    path: &mut PathBuf,
    producer: &mut rtrb::Producer<(f32, f32)>,
    cmd_rx: &Receiver<PlayerCommand>,
    meta_tx: &Sender<MetaCommand>,
    skip_samples: usize,
) {
    let mut sample_index = skip_samples;

    'main: loop {
        let file = File::open(&path).expect("Failed to open file");
        let mmap = unsafe { Mmap::map(&file) }.expect("Failed to mmap file");
        let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(mmap)), Default::default());

        let mut hint = Hint::new();
        hint.with_extension("flac");

        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .expect("Failed to probe media");

        let mut format = probed.format;
        let track = format.default_track().expect("No default track");
        let track_id = track.id;
        let sample_rate = track.codec_params.sample_rate.expect("No sample rate");
        let codec_params = track.codec_params.clone();

        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.sample_rate.is_some() && t.codec_params.n_frames.is_some())
            .expect("No suitable audio track found");

        let total_samples = track.codec_params.n_frames.unwrap();

        let duration_ms = (total_samples as u64) / sample_rate as u64;
        DURATION.store(duration_ms, Ordering::Relaxed);

        let mut decoder = symphonia::default::get_codecs()
            .make(&codec_params, &DecoderOptions::default())
            .expect("Failed to create decoder");

        if let Some(metadata) = format.metadata().current() {
            send_metadata(metadata, meta_tx);
        }

        if skip_samples > 0 {
            format
                .seek(
                    symphonia::core::formats::SeekMode::Accurate,
                    symphonia::core::formats::SeekTo::TimeStamp {
                        ts: skip_samples as u64,
                        track_id,
                    },
                )
                .expect("Failed to seek");
        }

        loop {
            if let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    PlayerCommand::ChangeSong(new_path) => {
                        fill_silence_buffer(producer);
                        *path = new_path;
                        sample_index = 0;
                        continue 'main;
                    }

                    PlayerCommand::SkipForward => {
                        let skip_to = sample_index + (sample_rate * 10) as usize;
                        format
                            .seek(
                                symphonia::core::formats::SeekMode::Coarse,
                                symphonia::core::formats::SeekTo::TimeStamp {
                                    ts: skip_to as u64,
                                    track_id,
                                },
                            )
                            .expect("Failed to seek");
                        sample_index = skip_to;
                        decoder = symphonia::default::get_codecs()
                            .make(
                                &codec_params,
                                &DecoderOptions {
                                    verify: false,
                                    ..Default::default()
                                },
                            )
                            .expect("Failed to create decoder");
                        continue;
                    }

                    PlayerCommand::SkipBackward => {
                        let skip_to = sample_index.saturating_sub((sample_rate * 10) as usize);
                        format
                            .seek(
                                symphonia::core::formats::SeekMode::Coarse,
                                symphonia::core::formats::SeekTo::TimeStamp {
                                    ts: skip_to as u64,
                                    track_id,
                                },
                            )
                            .expect("Failed to seek");
                        sample_index = skip_to;
                        decoder = symphonia::default::get_codecs()
                            .make(
                                &codec_params,
                                &DecoderOptions {
                                    verify: false,
                                    ..Default::default()
                                },
                            )
                            .expect("Failed to create decoder");
                        fill_silence_buffer(producer);
                        continue;
                    }
                }
            }

            if !IS_PLAYING.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_micros(100));
                continue;
            }

            match process_next_packet(
                &mut format,
                &mut decoder,
                producer,
                sample_rate,
                &mut sample_index,
            ) {
                Ok(()) => continue,
                Err(_) => continue 'main,
            }
        }
    }
}

fn perform_seek(format: &mut Box<dyn FormatReader>, ts: u64, track_id: u32) {
    format
        .seek(
            symphonia::core::formats::SeekMode::Accurate,
            symphonia::core::formats::SeekTo::TimeStamp { ts, track_id },
        )
        .expect("Failed to seek");
}

fn send_metadata(
    metadata: &symphonia::core::meta::MetadataRevision,
    meta_tx: &Sender<MetaCommand>,
) {
    let title = metadata
        .tags()
        .iter()
        .find(|tag| tag.std_key == Some(symphonia::core::meta::StandardTagKey::TrackTitle))
        .map(|tag| tag.value.to_string())
        .unwrap_or("EH".to_owned());

    let artist = metadata
        .tags()
        .iter()
        .find(|tag| tag.std_key == Some(symphonia::core::meta::StandardTagKey::Artist))
        .map(|tag| tag.value.to_string())
        .unwrap_or("AH".to_owned());
    meta_tx
        .send(MetaCommand::Metadata(title, artist))
        .expect("Failed to send metadata");
}

fn fill_silence_buffer(producer: &mut rtrb::Producer<(f32, f32)>) {
    let buffer_size = producer.slots();
    let silence_samples = (buffer_size / 2) as usize;
    for _ in 0..silence_samples {
        while producer.push((0.0, 0.0)).is_err() {
            thread::sleep(Duration::from_micros(10));
        }
    }
}

fn process_next_packet(
    format: &mut Box<dyn FormatReader>,
    decoder: &mut Box<dyn Decoder>,
    producer: &mut rtrb::Producer<(f32, f32)>,
    sample_rate: u32,
    sample_index: &mut usize,
) -> Result<(), Box<dyn Error>> {
    let packet = format.next_packet()?;

    CURRENT_POSITION.store((packet.ts() / sample_rate as u64) as u32, Ordering::Relaxed);

    let decoded = decoder.decode(&packet)?;
    let mut sample_buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
    sample_buffer.copy_interleaved_ref(decoded);

    for chunk in sample_buffer.samples().chunks_exact(2) {
        while producer.push((chunk[0], chunk[1])).is_err() {
            thread::sleep(Duration::from_micros(10));
        }
        *sample_index += 1;
    }

    Ok(())
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
            "xone.player.stop" => {
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
            "xone.player.1.skipforward" => {
                println!("SKIPPU");
                cmd_tx
                    .send(PlayerCommand::SkipForward)
                    .expect("Failed to send command");
            }
            "xone.player.1.skipbackward" => {
                println!("SKIPPU");
                cmd_tx
                    .send(PlayerCommand::SkipBackward)
                    .expect("Failed to send command");
            }

            _ => {}
        }
    }
}
