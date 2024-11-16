use crossbeam_channel::{bounded, Receiver, Sender};
use nats;
mod xonek2;
use eframe::egui;
use jack::{Client, PortFlags};
use std::{error::Error, vec};
use xonek2::*;

struct SarasvatiApp {}

impl Default for SarasvatiApp {
    fn default() -> Self {
        Self {}
    }
}

impl eframe::App for SarasvatiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading(
                egui::RichText::new("SARASVATI")
                    .size(50.0)
                    .strong()
                    .color(egui::Color32::WHITE),
            );
        });
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("Scanning for MIDI devices...");
    list_midi_ports_jack()?;
    let (nats_tx, nats_rx) = bounded(100);
    let (xone_tx, xone_rx) = bounded(100);

    // Spawn NATS thread
    std::thread::spawn(move || {
        if let Err(e) = run_nats(xone_tx, nats_rx) {
            eprintln!("NATS error: {}", e);
        }
    });

    std::thread::spawn(move || {
        println!("Initializing XoneK2...");
        let k2 = XoneK2::new("XONE:K2", nats_tx.clone(), xone_rx.clone());
    });

    let options = eframe::NativeOptions {
        ..Default::default()
    };

    eframe::run_native(
        "SARASVATI",
        options,
        Box::new(|_cc| Ok(Box::new(SarasvatiApp {}))),
    )
    .expect("Failed to start GUI");

    Ok(())
}

fn list_midi_ports_jack() -> Result<(), Box<dyn Error>> {
    let (client, _status) = Client::new("MidiPortLister", jack::ClientOptions::NO_START_SERVER)?;

    println!("\nSARASVASTI");
    println!("\nAvailable MIDI Input Ports:");
    println!("---------------------------");
    for (i, port) in client
        .ports(None, Some("midi"), PortFlags::IS_INPUT)
        .iter()
        .enumerate()
    {
        println!("{}: {}", i, port);
    }

    println!("\nAvailable MIDI Output Ports:");
    println!("----------------------------");
    for (i, port) in client
        .ports(None, Some("midi"), PortFlags::IS_OUTPUT)
        .iter()
        .enumerate()
    {
        println!("{}: {}", i, port);
    }

    println!();
    Ok(())
}

fn run_nats(
    nats_tx: Sender<XoneMessage>,
    nats_rx: Receiver<XoneMessage>,
) -> Result<(), Box<dyn Error>> {
    let nc = nats::connect("nats://localhost:4222")?;

    while let Ok(msg) = nats_rx.recv() {
        dbg!(&msg);
        match msg {
            XoneMessage::Fader { id, value } => {
                let _ = nc.publish("xone.fader", format!("{},{}", id, value));
            }
            XoneMessage::Encoder { id, direction } => {
                println!("ENCODER {}", id);
                if id == 14 {
                    let _ = nc.publish("xone.library", format!("{:?}", direction));
                } else {
                    let _ = nc.publish("xone.encoder", format!("{},{:?}", id, direction));
                }
            }
            XoneMessage::Button { id, pressed } => {
                println!("BUTTON {}-{}", id, pressed);

                match id {
                    14 => {
                        if pressed {
                            println!("select button pressed");
                            let _ = nc.publish("xone.library", "Select");
                        }
                    }
                    25 => {
                        if pressed {
                            nc.publish("xone.player.1.skipbackward", "na");
                        }
                    }

                    26 => {
                        if pressed {
                            nc.publish("xone.player.1.skipforward", "na");
                        }
                    }
                    _ => {}
                }

                let _ = nc.publish("xone.button", format!("{},{}", id, pressed));
            }
            XoneMessage::Knob { id, value } => {
                let _ = nc.publish("xone.knob", format!("{},{}", id, value));
            }
        }
    }

    Ok(())
}
