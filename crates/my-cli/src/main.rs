use crossbeam::channel::{bounded, Receiver, Sender};
use crossbeam::scope;
use midir::{MidiInput, MidiOutput};
use nats;
use std::error::Error;
use std::thread;
mod xonek2;
use xonek2::*;

fn main() -> Result<(), Box<dyn Error>> {
    println!("Scanning for MIDI devices...");
    list_midi_ports()?;

    let nc = nats::connect("nats://localhost:4222")?;
    let (nats_tx, nats_rx) = bounded(100); // Channel for sending to NATS
    let (xone_tx, xone_rx) = bounded(100); // Channel for receiving from NATS

    scope(|s| {
        // Spawn NATS handler within the crossbeam scope
        s.spawn(move |_| {
            if let Err(e) = run_nats(xone_tx, nats_rx) {
                eprintln!("NATS error: {}", e);
            }
        });

        println!("Initializing XoneK2...");
        let k2 = XoneK2::new("XONE:K2", nats_tx, xone_rx);
        dbg!(&k2);

        // Run XoneK2 handler
        k2.expect("k2").run();

        // Park the main thread to keep the application running
        std::thread::park();
    })
    .expect("Crossbeam scope error");

    Ok(())
}

fn list_midi_ports() -> Result<(), Box<dyn Error>> {
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
    let nc = nats::connect("nats://localhost:4222")?;

    // Handle publishing messages to NATS
    while let Ok(msg) = nats_rx.recv() {
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

    Ok(())
}
