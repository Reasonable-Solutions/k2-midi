use crossbeam::channel::{unbounded, Receiver, Sender};
use eframe::egui;
use eframe::egui::ColorImage;
use egui::{TextureHandle, TextureOptions};
use image::imageops::{self, FilterType};
use image::{load_from_memory, ImageReader};
use metaflac::{Block, Tag};
use nats::Connection;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::path::PathBuf;
use std::time::Duration;
use std::{fs, thread};

enum CounterMessage {
    Increment,
    Decrement,
    Select,
}

struct SelectMessage {
    file_path: String,
}

struct FlacFile {
    path: PathBuf,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    inline_album_art: Option<egui::TextureHandle>,
    large_album_art: Option<egui::TextureHandle>,
}
struct PlayerApp {
    playing: bool,
    ui_receiver: Receiver<CounterMessage>,
    select_sender: Sender<SelectMessage>,
}

impl PlayerApp {
    fn new(
        _cc: &eframe::CreationContext<'_>,
        ui_receiver: Receiver<CounterMessage>,
        select_sender: Sender<SelectMessage>,
    ) -> Self {
        Self {
            playing: false,
            ui_receiver,
            select_sender,
        }
    }

    fn toggle_playing(&mut self) {
        self.playing = !self.playing;
    }
}

fn main() {
    let (ui_sender, ui_receiver): (Sender<CounterMessage>, Receiver<CounterMessage>) = unbounded();
    let (select_sender, select_receiver): (Sender<SelectMessage>, Receiver<SelectMessage>) =
        unbounded();

    let nats_client = nats::connect("nats://localhost:4222").unwrap();
    let nats_client_for_thread = nats_client.clone();

    thread::spawn(move || {
        for message in nats_client_for_thread
            .subscribe("xone.>")
            .unwrap()
            .messages()
        {
            dbg!("{:?}", String::from_utf8_lossy(&message.data));
            match message.data.as_slice() {
                b"Clockwise" => ui_sender.send(CounterMessage::Increment).unwrap(),
                b"CounterClockwise" => ui_sender.send(CounterMessage::Decrement).unwrap(),
                b"Select" => ui_sender.send(CounterMessage::Select).unwrap(),
                _ => (),
            }
        }
    });

    thread::spawn(move || {
        while let Ok(select_message) = select_receiver.recv() {
            nats_client
                .publish("xone.player.1.select", select_message.file_path.as_bytes())
                .unwrap();
        }
    });

    eframe::run_native(
        "FLAC Player",
        eframe::NativeOptions::default(),
        Box::new(|cc| {
            Ok(Box::new(PlayerApp::new(
                cc,
                ui_receiver,
                select_sender,
            )))
        }),
    );
}

impl eframe::App for PlayerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Handle incoming messages
        while let Ok(message) = self.ui_receiver.try_recv() {
            match message {
                CounterMessage::Increment => self.playing = true,
                CounterMessage::Decrement => self.playing = false,
                CounterMessage::Select => {
                    // Example action on select, sending a message if playing
                    if self.playing {
                        let _ = self.select_sender.send(SelectMessage {
                            file_path: "example/path/to/selected/file".to_string(),
                        });
                    }
                }
            }
        }

        // UI rendering
        egui::CentralPanel::default().show(ctx, |ui| {
            // Display and toggle the "Playing" state
            let label = if self.playing { "Playing" } else { "Paused" };
            ui.colored_label(egui::Color32::YELLOW, label);

            // Button to toggle playing state
            if ui.button("Toggle Playing").clicked() {
                self.toggle_playing();
            }
        });
    }
}
    
