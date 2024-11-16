use crossbeam::channel::{bounded, Receiver, Sender};
use eframe::egui;
use jack::{AudioOut, Client, ClientOptions, Control, ProcessScope};
use memmap2::Mmap;
use nats;
use nats::kv;
use procfs::process::all_processes;
use rayon::prelude::*;
use rtrb::RingBuffer;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::{fs, thread};
use symphonia::core::audio::{AudioBuffer, AudioBufferRef, SampleBuffer, Signal};
use symphonia::core::codecs::{CodecRegistry, Decoder, DecoderOptions};
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub static ANAHATA_NO: AtomicU32 = AtomicU32::new(0);
pub static IS_PLAYING: AtomicBool = AtomicBool::new(true);
pub static CURRENT_POSITION: AtomicU64 = AtomicU64::new(0);
pub static DURATION: AtomicU64 = AtomicU64::new(0);
pub static PLAYHEAD: AtomicU64 = AtomicU64::new(0);

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
fn playback_thread(
    mut producer: rtrb::Producer<(f32, f32)>,
    mut song: Vec<(f32, f32)>,

    meta_tx: &Sender<MetaCommand>,
    cmd_rx: crossbeam::channel::Receiver<PlayerCommand>,
) {
    const SAMPLE_RATE: f64 = 48000.0;
    const MS_PER_SAMPLE: f64 = 1000.0 / SAMPLE_RATE; // ~0.0208333 ms per sample
    const SAMPLES_PER_SECOND: u64 = 48000;
    const SKIP_SECONDS: u64 = 5;

    loop {
        match cmd_rx.try_recv() {
            Ok(PlayerCommand::ChangeSong(path)) => {
                IS_PLAYING.store(false, Ordering::Relaxed);

                PLAYHEAD.store(0, Ordering::Relaxed);
                CURRENT_POSITION.store(0, Ordering::Relaxed);

                song = decode_flac_to_vec(&path, meta_tx);

                IS_PLAYING.store(true, Ordering::Relaxed);
            }
            Ok(PlayerCommand::SkipForward) => {
                let current_pos = PLAYHEAD.load(Ordering::Relaxed);
                let skip_amount = SKIP_SECONDS * SAMPLES_PER_SECOND;
                let new_pos = if (current_pos + skip_amount) as usize >= song.len() {
                    song.len() as u64
                } else {
                    current_pos + skip_amount
                };
                PLAYHEAD.store(new_pos, Ordering::Relaxed);
                let ms_position = (new_pos as f64 * MS_PER_SAMPLE) as u64;
                CURRENT_POSITION.store(ms_position, Ordering::Relaxed);
            }
            Ok(PlayerCommand::SkipBackward) => {
                let current_pos = PLAYHEAD.load(Ordering::Relaxed);
                let skip_amount = SKIP_SECONDS * SAMPLES_PER_SECOND;
                let new_pos = if current_pos <= skip_amount {
                    0
                } else {
                    current_pos - skip_amount
                };
                PLAYHEAD.store(new_pos, Ordering::Relaxed);
                let ms_position = (new_pos as f64 * MS_PER_SAMPLE) as u64;
                CURRENT_POSITION.store(ms_position, Ordering::Relaxed);
            }
            _ => {} // Handle other commands as before
        }

        if IS_PLAYING.load(Ordering::Relaxed) {
            let current_pos = PLAYHEAD.load(Ordering::Relaxed) as usize;
            if current_pos >= song.len() {
                PLAYHEAD.store(0, Ordering::Relaxed);
                CURRENT_POSITION.store(0, Ordering::Relaxed);
                continue;
            }

            let free_space = producer.slots();
            if free_space >= 1024 {
                let chunk_size = 1024.min(song.len() - current_pos);
                for i in 0..chunk_size {
                    if let Err(_) = producer.push(song[current_pos + i]) {
                        break;
                    }
                }
                PLAYHEAD.fetch_add(chunk_size as u64, Ordering::Relaxed);

                // Update CURRENT_POSITION (in milliseconds)
                let ms_position = (PLAYHEAD.load(Ordering::Relaxed) as f64 * MS_PER_SAMPLE) as u64;
                CURRENT_POSITION.store(ms_position, Ordering::Relaxed);
            }
        }
        thread::sleep(Duration::from_micros(1000));
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
            let number = ANAHATA_NO.load(Ordering::Relaxed);
            ui.heading(
                egui::RichText::new(format!("ANAHATA-{}", number))
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

    let start_time = Instant::now();
    let mut song = decode_flac_to_vec(&audio_file_path, &meta_tx);
    let elapsed_time = start_time.elapsed();
    println!("Decoding time: {:?}", elapsed_time);
    println!("LEN: {:?}", song.len());
    thread::spawn(move || {
        playback_thread(producer, song, &meta_tx, cmd_rx);
    });
    thread::spawn(move || {
        control_thread(cmd_tx);
    });

    let process_callback = move |_: &Client, ps: &ProcessScope| -> Control {
        let out_buffer_left = out_port_left.as_mut_slice(ps);
        let out_buffer_right = out_port_right.as_mut_slice(ps);

        if !IS_PLAYING.load(Ordering::Relaxed) {
            // If not playing, output silence. WE ARE ALWAYS PLAYING, SOMETIMES VERY SOFTLY
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

fn decode_flac_to_vec(path: &PathBuf, meta_tx: &Sender<MetaCommand>) -> Vec<(f32, f32)> {
    let file = File::open(path).expect("Failed to open file");
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

    // Collect all packets first
    let mut packets = Vec::new();
    while let Ok(packet) = format.next_packet() {
        packets.push(packet);
    }
    let track = format.default_track().expect("No default track");
    let codec_params = track.codec_params.clone();

    let track_id = track.id;

    let total_frames = track
        .codec_params
        .n_frames
        .expect("Could not determine total number of frames");
    dbg!(total_frames);

    let sample_format = track.codec_params.sample_format;
    // Process the collected packets in parallel
    let decoded_samples = packets
        .into_par_iter()
        .map_init(
            || {
                symphonia::default::get_codecs()
                    .make(&codec_params, &DecoderOptions::default())
                    .expect("Failed to create decoder")
            },
            |decoder, packet| {
                let mut samples = Vec::new();
                let decoded = decoder.decode(&packet).expect("Failed to decode packet");
                decode_audio_buffer(decoded, &mut samples);
                samples
            },
        )
        .reduce_with(|mut acc, chunk| {
            acc.extend(chunk);
            acc
        })
        .unwrap_or_default();

    println!("Decoded samples length: {}", decoded_samples.len());
    if let Some(metadata) = format.metadata().current() {
        send_metadata(metadata, meta_tx);
    }

    decoded_samples
}

fn decode_audio_buffer(decoded: AudioBufferRef<'_>, decoded_samples: &mut Vec<(f32, f32)>) {
    match decoded {
        AudioBufferRef::F32(buf) => {
            for (left, right) in buf.chan(0).iter().zip(buf.chan(1).iter()) {
                decoded_samples.push((*left, *right));
            }
        }
        AudioBufferRef::F64(buf) => {
            for (left, right) in buf.chan(0).iter().zip(buf.chan(1).iter()) {
                let left_sample = *left as f32;
                let right_sample = *right as f32;
                decoded_samples.push((left_sample, right_sample));
            }
        }
        AudioBufferRef::U16(buf) => {
            for (left, right) in buf.chan(0).iter().zip(buf.chan(1).iter()) {
                let left_sample = *left as f32 / u16::MAX as f32;
                let right_sample = *right as f32 / u16::MAX as f32;
                decoded_samples.push((left_sample, right_sample));
            }
        }
        AudioBufferRef::U32(buf) => {
            for (left, right) in buf.chan(0).iter().zip(buf.chan(1).iter()) {
                let left_sample = *left as f32 / u32::MAX as f32;
                let right_sample = *right as f32 / u32::MAX as f32;
                decoded_samples.push((left_sample, right_sample));
            }
        }
        AudioBufferRef::U8(buf) => {
            for (left, right) in buf.chan(0).iter().zip(buf.chan(1).iter()) {
                let left_sample = (*left as f32 - 128.0) / 128.0;
                let right_sample = (*right as f32 - 128.0) / 128.0;
                decoded_samples.push((left_sample, right_sample));
            }
        }
        AudioBufferRef::S8(buf) => {
            for (left, right) in buf.chan(0).iter().zip(buf.chan(1).iter()) {
                let left_sample = *left as f32 / 128.0;
                let right_sample = *right as f32 / 128.0;
                decoded_samples.push((left_sample, right_sample));
            }
        }
        AudioBufferRef::S16(buf) => {
            for (left, right) in buf.chan(0).iter().zip(buf.chan(1).iter()) {
                let left_sample = *left as f32 / i16::MAX as f32;
                let right_sample = *right as f32 / i16::MAX as f32;
                decoded_samples.push((left_sample, right_sample));
            }
        }
        AudioBufferRef::U24(buf) => {
            for (left, right) in buf.chan(0).iter().zip(buf.chan(1).iter()) {
                let left_sample = (left.inner() as f32) / 8388607.0;
                let right_sample = (right.inner() as f32) / 8388607.0;
                decoded_samples.push((left_sample, right_sample));
            }
        }
        AudioBufferRef::S24(buf) => {
            for (left, right) in buf.chan(0).iter().zip(buf.chan(1).iter()) {
                let left_sample = (left.inner() as i32 as f32) / 8388607.0;
                let right_sample = (right.inner() as i32 as f32) / 8388607.0;
                decoded_samples.push((left_sample, right_sample));
            }
        }

        AudioBufferRef::S32(buf) => {
            for (left, right) in buf.chan(0).iter().zip(buf.chan(1).iter()) {
                let left_sample = *left as f32 / i32::MAX as f32;
                let right_sample = *right as f32 / i32::MAX as f32;
                decoded_samples.push((left_sample, right_sample));
            }
        }
    }
}
