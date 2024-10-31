use crossbeam_channel::{bounded, Receiver, Sender};
use midir::{MidiInput, MidiOutput};
use std::error::Error;
use std::future::Future;
use std::thread::{self};
mod xonek2;
use xonek2::*;

use nats;

fn main() -> Result<(), Box<dyn Error>> {
    println!("Scanning for MIDI devices...");
    list_midi_ports()?;
    let nc: _ = nats::connect("nats://localhost:4222");

    let (nats_tx, nats_rx) = bounded(100); // Channel for sending to NATS
    let (xone_tx, xone_rx) = bounded(100); // Channel for receiving from NATS

    // Spawn NATS thread
    let nats_handle = thread::spawn(move || {
        if let Err(e) = run_nats(xone_tx, nats_rx) {
            eprintln!("NATS error: {}", e);
        }
    });
    print!("run");
    let k2 = XoneK2::new("XONE:K2", nats_tx, xone_rx).unwrap();
    dbg!(&k2);
    k2.run()?;
    // Keep the application running
    std::thread::park();

    Ok(())
}

fn list_midi_ports() -> Result<(), Box<dyn Error>> {
    // Create MIDI input and output handles
    let midi_in = MidiInput::new("MIDI Input")?;
    let midi_out = MidiOutput::new("MIDI Output")?;

    println!("\nAvailable MIDI Input Ports:");
    println!("---------------------------");
    let in_ports = midi_in.ports();
    for (i, p) in in_ports.iter().enumerate() {
        println!("{}: {}", i, midi_in.port_name(p)?);
    }

    println!("\nAvailable MIDI Output Ports:");
    println!("----------------------------");
    let out_ports = midi_out.ports();
    for (i, p) in out_ports.iter().enumerate() {
        println!("{}: {}", i, midi_out.port_name(p)?);
    }

    println!(); // Empty line for better readability
    Ok(())
}

// NATS handler function
fn run_nats(
    nats_tx: Sender<XoneMessage>,
    nats_rx: Receiver<XoneMessage>,
) -> Result<(), Box<dyn Error>> {
    // Connect to NATS synchronously
    let nc = nats::connect("nats://localhost:4222")?;

    // Spawn a thread for publishing messages
    thread::spawn(move || {
        while let Ok(msg) = nats_rx.recv() {
            // Convert XoneMessage to NATS message
            match msg {
                XoneMessage::Fader { id, value } => {
                    let _ = nc.publish("xone.fader", format!("{},{}", id, value));
                }
                XoneMessage::Encoder { id, direction } => {
                    let _ = nc.publish("xone.encoder", format!("{},{:?}", id, direction));
                }
                XoneMessage::Button { id, pressed } => {
                    let _ = nc.publish("xone.button", format!("{},{}", id, pressed));
                }
                XoneMessage::Knob { id, value } => {
                    let _ = nc.publish("xone.knob", format!("{},{}", id, value));
                }
            }
        }
    });

    Ok(())
}
