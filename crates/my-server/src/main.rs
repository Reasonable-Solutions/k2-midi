#![warn(clippy::all, rust_2018_idioms)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release
use crossbeam::channel::{unbounded, Receiver};
use eframe::egui;
use nats::Connection;
use std::{thread, time::Duration};

enum CounterMessage {
    Increment,
    Decrement,
}

fn main() -> eframe::Result {
    env_logger::init(); // Log to stderr (if you run with `RUST_LOG=debug`).
    let (sender, receiver) = unbounded();

    let nats_client = nats::connect("nats://localhost:4222").unwrap();

    thread::spawn(move || {
        for message in nats_client.subscribe("xone.library").unwrap().messages() {
            // Parse message to determine action (increment or decrement)
            match message.data.as_slice() {
                b"Clockwise" => sender.send(CounterMessage::Increment).unwrap(),
                b"CounterClockwise" => sender.send(CounterMessage::Decrement).unwrap(),
                _ => (),
            }
        }
    });

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 300.0])
            .with_min_inner_size([300.0, 220.0]),
        ..Default::default()
    };
    eframe::run_native(
        "eframe template",
        native_options,
        Box::new(|cc| Ok(Box::new(MyApp::new(cc, receiver)))),
    )
}

#[derive(Debug, Default)]
struct State {
    select_index: u16,
}

struct MyApp {
    receiver: Receiver<CounterMessage>,
    ui_state: State,
}

impl MyApp {
    fn new(_cc: &eframe::CreationContext<'_>, receiver: Receiver<CounterMessage>) -> Self {
        Self {
            receiver,
            ui_state: State::default(),
        }
    }
}

impl eframe::App for MyApp {
    /// Called each time the UI needs repainting, which may be many times per second.
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Put your widgets into a `SidePanel`, `TopBottomPanel`, `CentralPanel`, `Window` or `Area`.
        // For inspiration and more examples, go to https://emilk.github.io/egui
        while let Ok(message) = self.receiver.try_recv() {
            match message {
                CounterMessage::Increment => {
                    self.ui_state.select_index = self.ui_state.select_index.saturating_add(1);
                }
                CounterMessage::Decrement => {
                    self.ui_state.select_index = self.ui_state.select_index.saturating_sub(1);
                }
            }
        }
        ctx.request_repaint_after(Duration::from_millis(12));
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            // The top panel is often a good place for a menu bar:

            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.add_space(16.0);

                egui::widgets::global_theme_preference_buttons(ui);
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            // The central panel the region left after adding TopPanel's and SidePanel's
            ui.heading("eframe template");

            ui.separator();
            ui.heading(self.ui_state.select_index.to_string());
            ["file1", "file2"].map(|f| ui.heading(f));
            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                powered_by_egui_and_eframe(ui);
                egui::warn_if_debug_build(ui);
            });
        });
    }
}

fn powered_by_egui_and_eframe(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.label("Powered by ");
        ui.hyperlink_to("egui", "https://github.com/emilk/egui");
        ui.label(" and ");
        ui.hyperlink_to(
            "eframe",
            "https://github.com/emilk/egui/tree/master/crates/eframe",
        );
        ui.label(".");
    });
}
