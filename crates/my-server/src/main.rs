use crossbeam::channel::{unbounded, Receiver, Sender};
use eframe::egui;
use nats::Connection;
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

fn main() {
    let (ui_sender, ui_receiver): (Sender<CounterMessage>, Receiver<CounterMessage>) = unbounded();
    let (select_sender, select_receiver): (Sender<SelectMessage>, Receiver<SelectMessage>) =
        unbounded();

    let nats_client = nats::connect("nats://localhost:4222").unwrap();
    let nats_publisher = nats_client.clone();

    thread::spawn(move || {
        for message in nats_client.subscribe("xone.library").unwrap().messages() {
            match message.data.as_slice() {
                b"Clockwise" => ui_sender.send(CounterMessage::Increment).unwrap(),
                b"CounterClockwise" => ui_sender.send(CounterMessage::Decrement).unwrap(),
                b"select" => ui_sender.send(CounterMessage::Select).unwrap(),
                any_other => {
                    dbg!(any_other);
                }
            }
        }
    });

    thread::spawn(move || {
        while let Ok(select_message) = select_receiver.recv() {
            dbg!(&select_message.file_path);
            nats_publisher
                .publish("xone.library", select_message.file_path.as_bytes())
                .unwrap();
        }
    });

    eframe::run_native(
        "FLAC File Selector",
        eframe::NativeOptions::default(),
        Box::new(|cc| {
            Ok(Box::new(FileSelectorApp::new(
                cc,
                ui_receiver,
                select_sender,
            )))
        }),
    );
}

struct FileSelectorApp {
    ui_receiver: Receiver<CounterMessage>,
    select_sender: Sender<SelectMessage>,
    flac_files: Vec<PathBuf>,
    selected_index: usize,
}

impl FileSelectorApp {
    fn new(
        _cc: &eframe::CreationContext<'_>,
        ui_receiver: Receiver<CounterMessage>,
        select_sender: Sender<SelectMessage>,
    ) -> Self {
        let flac_files = fs::read_dir("./music")
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.extension().map(|ext| ext == "flac").unwrap_or(false) {
                    Some(path)
                } else {
                    None
                }
            })
            .collect();

        Self {
            ui_receiver,
            select_sender,
            flac_files,
            selected_index: 0,
        }
    }
}

impl eframe::App for FileSelectorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(message) = self.ui_receiver.try_recv() {
            match message {
                CounterMessage::Increment => {
                    if self.selected_index < self.flac_files.len() - 1 {
                        self.selected_index += 1;
                    }
                }
                CounterMessage::Decrement => {
                    if self.selected_index > 0 {
                        self.selected_index -= 1;
                    }
                }
                CounterMessage::Select => {
                    if let Some(selected_file) = self.flac_files.get(self.selected_index) {
                        let file_path = selected_file.to_string_lossy().to_string();
                        self.select_sender
                            .send(SelectMessage { file_path })
                            .unwrap();
                    }
                }
            }
        }
        // TODO WTF EGUI NOT COOL
        ctx.request_repaint_after(Duration::from_millis(12));

        egui::CentralPanel::default().show(ctx, |ui| {
            for (i, file) in self.flac_files.iter().enumerate() {
                let label = file.file_name().unwrap().to_string_lossy();
                if i == self.selected_index {
                    ui.colored_label(egui::Color32::YELLOW, label);
                } else {
                    ui.label(label);
                }
            }
        });
    }
}
