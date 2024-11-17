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
    Waveform(Vec<WaveformBin>),
}

struct PlayerApp {
    current_title: String,
    current_artist: String,
    waveform: Vec<WaveformBin>,
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
    fn viridis_color(amplitude: f32) -> egui::Color32 {
        let amp = amplitude.clamp(0.0, 1.0);

        // More vivid Viridis implementation
        if amp < 0.25 {
            // Dark blue to blue
            let t = amp * 4.0;
            egui::Color32::from_rgb(
                (68.0 + t * 32.0) as u8,
                (1.0 + t * 100.0) as u8,
                (84.0 + t * 100.0) as u8,
            )
        } else if amp < 0.5 {
            // Blue to green
            let t = (amp - 0.25) * 4.0;
            egui::Color32::from_rgb(
                (100.0 - t * 20.0) as u8,
                (101.0 + t * 154.0) as u8,
                (184.0 - t * 84.0) as u8,
            )
        } else if amp < 0.75 {
            // Green to yellow-green
            let t = (amp - 0.5) * 4.0;
            egui::Color32::from_rgb(
                (80.0 + t * 175.0) as u8,
                (255.0) as u8,
                (100.0 - t * 50.0) as u8,
            )
        } else {
            // Yellow-green to yellow
            let t = (amp - 0.75) * 4.0;
            egui::Color32::from_rgb((255.0) as u8, (255.0) as u8, (50.0 - t * 50.0) as u8)
        }
    }

    fn draw_detailed_waveform(&mut self, ui: &mut egui::Ui) {
        let waveform_height = 400.0;
        let pixels_per_second = 10.0;
        let duration_secs = DURATION.load(Ordering::Relaxed) as f32 / 1000.0;
        let total_width = duration_secs * pixels_per_second;

        egui::ScrollArea::horizontal()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let detailed_response = ui.allocate_response(
                    egui::vec2(total_width, waveform_height),
                    egui::Sense::click_and_drag(),
                );

                if !self.waveform.is_empty() {
                    let rect = detailed_response.rect;
                    let painter = ui.painter();

                    let width_per_bin = total_width / self.waveform.len() as f32;
                    let playhead_x = ui.clip_rect().center().x;

                    let current_bin = (CURRENT_POSITION.load(Ordering::Relaxed) as f32
                        / DURATION.load(Ordering::Relaxed) as f32
                        * self.waveform.len() as f32)
                        as usize;
                    let now = std::time::Instant::now();
                    // Only update scroll position every 16ms (roughly 60fps)
                    if now.duration_since(self.last_update).as_millis() >= 16 {
                        self.smooth_offset = -(current_bin as f32 * width_per_bin) + playhead_x;
                        self.last_update = now;
                    }

                    // Use the stored scroll offset instead of calculating it every frame
                    let scroll_offset = self.smooth_offset;

                    // Draw waveform bins from back to front
                    for (i, bin) in self.waveform.iter().enumerate() {
                        let x = rect.left() + (i as f32 * width_per_bin) + scroll_offset;

                        if x >= ui.clip_rect().left() - width_per_bin
                            && x <= ui.clip_rect().right() + width_per_bin
                        {
                            let center_y = rect.center().y;

                            // Scale RMS values and map to colors
                            let low_rms = (bin.low.rms_left + bin.low.rms_right) * 0.5;
                            let mid_rms = 0.4 + (bin.mid.rms_left + bin.mid.rms_right) * 0.3;
                            let high_rms = 0.7 + (bin.high.rms_left + bin.high.rms_right) * 0.3;

                            let low_color = Self::viridis_color(low_rms);
                            let mid_color = Self::viridis_color(mid_rms);
                            let high_color = Self::viridis_color(high_rms);

                            // Draw from back to front with different widths
                            // Low frequencies (widest)
                            painter.line_segment(
                                [
                                    egui::pos2(
                                        x,
                                        center_y - bin.low.rms_left * waveform_height * 0.6,
                                    ),
                                    egui::pos2(
                                        x,
                                        center_y + bin.low.rms_right * waveform_height * 0.6,
                                    ),
                                ],
                                egui::Stroke::new(3.0, low_color),
                            );

                            // Mid frequencies (medium)
                            painter.line_segment(
                                [
                                    egui::pos2(
                                        x,
                                        center_y - bin.mid.rms_left * waveform_height * 0.7,
                                    ),
                                    egui::pos2(
                                        x,
                                        center_y + bin.mid.rms_right * waveform_height * 0.7,
                                    ),
                                ],
                                egui::Stroke::new(2.0, mid_color),
                            );

                            // High frequencies (thinnest)
                            painter.line_segment(
                                [
                                    egui::pos2(
                                        x,
                                        center_y - bin.high.rms_left * waveform_height * 0.30,
                                    ),
                                    egui::pos2(
                                        x,
                                        center_y + bin.high.rms_right * waveform_height * 0.30,
                                    ),
                                ],
                                egui::Stroke::new(1.0, high_color),
                            );
                        }
                    }

                    // Draw centered playhead
                    painter.line_segment(
                        [
                            egui::pos2(playhead_x, rect.top()),
                            egui::pos2(playhead_x, rect.bottom()),
                        ],
                        egui::Stroke::new(2.0, egui::Color32::WHITE),
                    );

                    // Handle interactions
                    if detailed_response.dragged() {
                        let drag_delta = detailed_response.drag_delta();
                        let time_per_pixel = duration_secs / total_width;
                        let time_delta = -drag_delta.x * time_per_pixel;

                        let new_pos = (CURRENT_POSITION.load(Ordering::Relaxed) as f32
                            + time_delta * 1000.0) as u64;
                        let new_pos = new_pos.clamp(0, DURATION.load(Ordering::Relaxed));

                        let new_samples = ((new_pos as f64 / 1000.0) * 48000.0) as u64;
                        PLAYHEAD.store(new_samples, Ordering::Relaxed);
                        CURRENT_POSITION.store(new_pos, Ordering::Relaxed);
                    }

                    // Hover effect
                    if detailed_response.hovered() {
                        if let Some(hover_pos) = detailed_response.hover_pos() {
                            painter.line_segment(
                                [
                                    egui::pos2(hover_pos.x, rect.top()),
                                    egui::pos2(hover_pos.x, rect.bottom()),
                                ],
                                egui::Stroke::new(1.0, egui::Color32::from_white_alpha(100)),
                            );
                        }
                    }
                }
            });
    }

    fn draw_overview_waveform(&self, ui: &mut egui::Ui) {
        let overview_height = 50.0;
        let response = ui.allocate_response(
            egui::vec2(ui.available_width(), overview_height),
            egui::Sense::click_and_drag(),
        );

        if !self.waveform.is_empty() {
            let rect = response.rect;
            let painter = ui.painter();
            let width_per_bin = rect.width() / self.waveform.len() as f32;

            for (i, bin) in self.waveform.iter().enumerate() {
                let x = rect.left() + (i as f32 * width_per_bin);
                let center_y = rect.center().y;

                // Different intensity ranges for each band
                // Lows: 0.0 -> 0.33 (darker/cooler colors)
                let low_rms = (bin.low.rms_left + bin.low.rms_right) * 0.5 * 0.33;
                let low_color = Self::viridis_color(low_rms);

                // Mids: 0.33 -> 0.66 (middle range colors)
                let mid_rms = 0.33 + (bin.mid.rms_left + bin.mid.rms_right) * 0.5 * 0.33;
                let mid_color = Self::viridis_color(mid_rms);

                // Highs: 0.66 -> 1.0 (brighter/warmer colors)
                let high_rms = 0.66 + (bin.high.rms_left + bin.high.rms_right) * 0.5 * 0.34;
                let high_color = Self::viridis_color(high_rms);

                // Draw from back to front
                painter.line_segment(
                    [
                        egui::pos2(x, center_y - bin.low.rms_left * overview_height * 0.45),
                        egui::pos2(x, center_y + bin.low.rms_right * overview_height * 0.45),
                    ],
                    egui::Stroke::new(3.0, low_color),
                );

                painter.line_segment(
                    [
                        egui::pos2(x, center_y - bin.mid.rms_left * overview_height * 0.35),
                        egui::pos2(x, center_y + bin.mid.rms_right * overview_height * 0.35),
                    ],
                    egui::Stroke::new(2.0, mid_color),
                );

                painter.line_segment(
                    [
                        egui::pos2(x, center_y - bin.high.rms_left * overview_height * 0.25),
                        egui::pos2(x, center_y + bin.high.rms_right * overview_height * 0.25),
                    ],
                    egui::Stroke::new(1.0, high_color),
                );
            }

            // Draw playhead
            let current_pos = CURRENT_POSITION.load(Ordering::Relaxed) as f32;
            let duration = DURATION.load(Ordering::Relaxed) as f32;
            let playhead_x = rect.left() + (current_pos / duration) * rect.width();

            painter.line_segment(
                [
                    egui::pos2(playhead_x, rect.top()),
                    egui::pos2(playhead_x, rect.bottom()),
                ],
                egui::Stroke::new(2.0, egui::Color32::WHITE),
            );

            // Hover effect
            if response.hovered() {
                if let Some(hover_pos) = response.hover_pos() {
                    painter.line_segment(
                        [
                            egui::pos2(hover_pos.x, rect.top()),
                            egui::pos2(hover_pos.x, rect.bottom()),
                        ],
                        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(100)),
                    );
                }
            }

            // Handle interactions
            if response.clicked() || response.dragged() {
                if let Some(pos) = response.interact_pointer_pos() {
                    let fraction = (pos.x - rect.left()) / rect.width();
                    let new_pos = (fraction * duration) as u64;
                    let new_samples = ((new_pos as f64 / 1000.0) * 48000.0) as u64;

                    PLAYHEAD.store(new_samples, Ordering::Relaxed);
                    CURRENT_POSITION.store(new_pos, Ordering::Relaxed);
                }
            }
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
                MetaCommand::Waveform(wf) => self.waveform = wf,
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            let number = ANAHATA_NO.load(Ordering::Relaxed);
            ui.horizontal(|ui| {
                ui.heading(
                    egui::RichText::new(format!("ANAHATA-{}", number))
                        .size(50.0)
                        .strong()
                        .color(egui::Color32::WHITE),
                );
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
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
                    });
                    ui.vertical(|ui| {
                        ui.heading(&self.current_title);
                        ui.heading(&self.current_artist);
                    });
                });
            });
            self.draw_overview_waveform(ui);
            self.draw_detailed_waveform(ui);
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

                let wf = generate_waveform(&song, 20000);
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

#[derive(Debug)]
struct FrequencyBand {
    rms_left: f32,
    rms_right: f32,
    peak_left: f32,
    peak_right: f32,
}

#[derive(Debug)]
struct WaveformBin {
    low: FrequencyBand,  // 20-200Hz (sub bass and bass)
    mid: FrequencyBand,  // 200-3000Hz (kicks, snares, vocals)
    high: FrequencyBand, // 3000-20000Hz (hi-hats, cymbals, air)
}

fn generate_waveform(song: &Vec<(f32, f32)>, num_bins: usize) -> Vec<WaveformBin> {
    if song.is_empty() {
        return vec![];
    }

    let samples_per_bin = (song.len() + num_bins - 1) / num_bins;
    let mut waveform = Vec::with_capacity(num_bins);

    // Formula used: coeff = exp(-2.0 * PI * (cutoff_freq / sample_rate))
    // For 48kHz:
    // 300Hz -> exp(-2π * (300/48000)) ≈ 0.9961
    // 4kHz  -> exp(-2π * (4000/48000)) ≈ 0.9484

    let low_coeff = 0.9961f32; // ~300Hz cutoff

    // Mid band: 300Hz - 4kHz
    let mid_low_coeff = 0.9961f32; // ~300Hz
    let mid_high_coeff = 0.9484f32; // ~4kHz

    // High band: 4kHz - 20kHz
    let high_coeff = 0.9484f32; // ~4kHz

    for bin in 0..num_bins {
        let chunk_start = bin * samples_per_bin;
        let chunk_end = (chunk_start + samples_per_bin).min(song.len());

        if chunk_start >= song.len() {
            waveform.push(WaveformBin {
                low: FrequencyBand {
                    rms_left: 0.0,
                    rms_right: 0.0,
                    peak_left: 0.0,
                    peak_right: 0.0,
                },
                mid: FrequencyBand {
                    rms_left: 0.0,
                    rms_right: 0.0,
                    peak_left: 0.0,
                    peak_right: 0.0,
                },
                high: FrequencyBand {
                    rms_left: 0.0,
                    rms_right: 0.0,
                    peak_left: 0.0,
                    peak_right: 0.0,
                },
            });
            continue;
        }

        let mut low_sum_l = 0.0f32;
        let mut low_sum_r = 0.0f32;
        let mut mid_sum_l = 0.0f32;
        let mut mid_sum_r = 0.0f32;
        let mut high_sum_l = 0.0f32;
        let mut high_sum_r = 0.0f32;

        let mut low_peak_l = 0.0f32;
        let mut low_peak_r = 0.0f32;
        let mut mid_peak_l = 0.0f32;
        let mut mid_peak_r = 0.0f32;
        let mut high_peak_l = 0.0f32;
        let mut high_peak_r = 0.0f32;

        // State variables for filters
        let mut low_l = 0.0f32;
        let mut low_r = 0.0f32;
        let mut mid_low_l = 0.0f32;
        let mut mid_low_r = 0.0f32;
        let mut mid_high_l = 0.0f32;
        let mut mid_high_r = 0.0f32;
        let mut high_l = 0.0f32;
        let mut high_r = 0.0f32;

        // Process the chunk with DJ-style crossovers
        for &(left, right) in &song[chunk_start..chunk_end] {
            // Low band (below 200Hz)
            low_l = low_l * low_coeff + left * (1.0 - low_coeff);
            low_r = low_r * low_coeff + right * (1.0 - low_coeff);

            // Mid band (200Hz-3kHz): band-pass using two filters
            mid_low_l = mid_low_l * mid_low_coeff + left * (1.0 - mid_low_coeff);
            mid_low_r = mid_low_r * mid_low_coeff + right * (1.0 - mid_low_coeff);
            mid_high_l = mid_high_l * mid_high_coeff + left * (1.0 - mid_high_coeff);
            mid_high_r = mid_high_r * mid_high_coeff + right * (1.0 - mid_high_coeff);

            let mid_l = mid_high_l - mid_low_l;
            let mid_r = mid_high_r - mid_low_r;

            // High band (above 3kHz)
            high_l = left - (high_l * high_coeff + left * (1.0 - high_coeff));
            high_r = right - (high_r * high_coeff + right * (1.0 - high_coeff));

            // Accumulate RMS
            low_sum_l += low_l * low_l;
            low_sum_r += low_r * low_r;
            mid_sum_l += mid_l * mid_l;
            mid_sum_r += mid_r * mid_r;
            high_sum_l += high_l * high_l;
            high_sum_r += high_r * high_r;

            // Track peaks
            low_peak_l = low_peak_l.max(low_l.abs());
            low_peak_r = low_peak_r.max(low_r.abs());
            mid_peak_l = mid_peak_l.max(mid_l.abs());
            mid_peak_r = mid_peak_r.max(mid_r.abs());
            high_peak_l = high_peak_l.max(high_l.abs());
            high_peak_r = high_peak_r.max(high_r.abs());
        }

        let n = (chunk_end - chunk_start) as f32;

        waveform.push(WaveformBin {
            low: FrequencyBand {
                rms_left: (low_sum_l / n).sqrt(),
                rms_right: (low_sum_r / n).sqrt(),
                peak_left: low_peak_l,
                peak_right: low_peak_r,
            },
            mid: FrequencyBand {
                rms_left: (mid_sum_l / n).sqrt(),
                rms_right: (mid_sum_r / n).sqrt(),
                peak_left: mid_peak_l,
                peak_right: mid_peak_r,
            },
            high: FrequencyBand {
                rms_left: (high_sum_l / n).sqrt(),
                rms_right: (high_sum_r / n).sqrt(),
                peak_left: high_peak_l,
                peak_right: high_peak_r,
            },
        });
    }

    waveform
}
