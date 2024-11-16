use crossbeam::channel::{bounded, Receiver, Sender};
use eframe::egui;
use jack::{AudioOut, Client, ClientOptions, Control, ProcessScope};
use memmap2::Mmap;
use nats;
use rayon::prelude::*;
use rtrb::RingBuffer;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};
use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

mod globals;
use crate::globals::*;

#[derive(Debug)]
enum PlayerCommand {
    ChangeSong(PathBuf),
    SkipForward,
    SkipBackward,
}
#[derive(Debug)]
enum MetaCommand {
    Metadata(String, String),
    Waveform(Vec<(f32, f32)>),
}

struct PlayerApp {
    current_title: String,
    current_artist: String,
    waveform: Vec<(f32, f32)>,
    meta_rx: Receiver<MetaCommand>,
    smooth_offset: f32,
    last_update: std::time::Instant,
}

impl PlayerApp {
    fn new(cc: &eframe::CreationContext, meta_rx: Receiver<MetaCommand>) -> Self {
        Self {
            current_title: String::from("Unknown"),
            current_artist: String::from("Unknown"),
            waveform: Vec::new(),
            meta_rx,
            smooth_offset: 0.0,
            last_update: std::time::Instant::now(),
        }
    }
    fn amplitude_to_color(amplitude: f32) -> egui::Color32 {
        let amp = amplitude.clamp(0.0, 1.0);
        if amp < 0.5 {
            let t = amp * 2.0;
            egui::Color32::from_rgb(
                (68.0 + t * 54.0) as u8,
                (1.0 + t * 135.0) as u8,
                (84.0 + t * 47.0) as u8,
            )
        } else {
            let t = (amp - 0.5) * 2.0;
            egui::Color32::from_rgb(
                (122.0 + t * 131.0) as u8,
                (136.0 + t * 87.0) as u8,
                (131.0 - t * 109.0) as u8,
            )
        }
    }
    fn draw_waveform(&mut self, ui: &mut egui::Ui) {
        let waveform_height = 100.0;
        let waveform_response = ui.allocate_response(
            egui::vec2(ui.available_width(), waveform_height),
            egui::Sense::drag(),
        );

        if !self.waveform.is_empty() {
            let rect = waveform_response.rect;
            let painter = ui.painter();

            let width_per_bin = rect.width() / self.waveform.len() as f32;
            let playhead_x = rect.center().x;
            let target_bin = (CURRENT_POSITION.load(Ordering::Relaxed) as f32
                / DURATION.load(Ordering::Relaxed) as f32
                * self.waveform.len() as f32) as f32;

            // Smooth animation
            let now = std::time::Instant::now();
            let dt = now.duration_since(self.last_update).as_secs_f32();
            self.last_update = now;

            // interpolate
            let animation_speed = 10.0;
            self.smooth_offset += (target_bin - self.smooth_offset) * (animation_speed * dt);

            for (i, &(left, right)) in self.waveform.iter().enumerate() {
                let bin_offset = i as f32 - self.smooth_offset;
                let x = playhead_x + (bin_offset * width_per_bin);

                if x >= rect.left() && x <= rect.right() {
                    // Left channel (top half)
                    let left_y_top = rect.center().y;
                    let left_y_bottom = left_y_top - (left * waveform_height * 0.45);
                    let left_color = Self::amplitude_to_color(left);
                    painter.line_segment(
                        [egui::pos2(x, left_y_top), egui::pos2(x, left_y_bottom)],
                        egui::Stroke::new(1.0, left_color),
                    );

                    // Right channel (bottom half)
                    let right_y_top = rect.center().y;
                    let right_y_bottom = right_y_top + (right * waveform_height * 0.45);
                    let right_color = Self::amplitude_to_color(right);
                    painter.line_segment(
                        [egui::pos2(x, right_y_top), egui::pos2(x, right_y_bottom)],
                        egui::Stroke::new(1.0, right_color),
                    );
                }
            }

            painter.line_segment(
                [
                    egui::pos2(playhead_x, rect.top()),
                    egui::pos2(playhead_x, rect.bottom()),
                ],
                egui::Stroke::new(2.0, egui::Color32::WHITE),
            );

            if waveform_response.dragged() {
                let drag_delta = waveform_response.drag_delta();
                let bins_delta = drag_delta.x / width_per_bin;
                self.smooth_offset -= bins_delta;

                let time_per_bin =
                    DURATION.load(Ordering::Relaxed) as f32 / self.waveform.len() as f32;
                let new_pos = ((self.smooth_offset as f32 * time_per_bin) as u64)
                    .clamp(0, DURATION.load(Ordering::Relaxed));

                let new_samples = ((new_pos as f64 / 1000.0) * 48000.0) as u64;
                PLAYHEAD.store(new_samples, Ordering::Relaxed);
                CURRENT_POSITION.store(new_pos, Ordering::Relaxed);
            }
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

                let wf = generate_stereo_waveform(&song, 2000);
                let _ = meta_tx.send(MetaCommand::Waveform(wf));

                let total_samples = song.len() as f64;
                let duration_ms = (total_samples * MS_PER_SAMPLE) as u64;
                DURATION.store(duration_ms, Ordering::Relaxed);
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
            _ => {}
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
                MetaCommand::Waveform(wf) => self.waveform = wf,
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

            self.draw_waveform(ui);

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

    let mut song = Vec::new();

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

    let sub = nc.subscribe("xone.>").expect("Failed to subscribe topic");

    println!("Control thread started, listening for NATS messages");

    for msg in sub.messages() {
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
                cmd_tx
                    .send(PlayerCommand::SkipForward)
                    .expect("Failed to send command");
            }
            "xone.player.1.skipbackward" => {
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

fn generate_stereo_waveform(song: &Vec<(f32, f32)>, num_bins: usize) -> Vec<(f32, f32)> {
    if song.is_empty() {
        return vec![];
    }

    let samples_per_bin = (song.len() + num_bins - 1) / num_bins;
    let mut waveform = Vec::with_capacity(num_bins);

    for bin in 0..num_bins {
        let chunk_start = bin * samples_per_bin;
        let chunk_end = (chunk_start + samples_per_bin).min(song.len());

        // Find max amplitude for left and right channels separately
        let max_amplitudes = if chunk_start < song.len() {
            song[chunk_start..chunk_end]
                .iter()
                .fold((0.0f32, 0.0f32), |acc, &(l, r)| {
                    (acc.0.max(l.abs()), acc.1.max(r.abs()))
                })
        } else {
            // Pad with zeros
            (0.0, 0.0)
        };

        waveform.push(max_amplitudes);
    }

    waveform
}
