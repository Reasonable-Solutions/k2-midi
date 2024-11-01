use crossbeam_channel::{bounded, Receiver, Sender};
use nats;
mod xonek2;
use jack::{Client, PortFlags};
use std::{error::Error, vec};
use xonek2::*;

fn main() -> Result<(), Box<dyn Error>> {
    println!("Scanning for MIDI devices...");
    list_midi_ports_jack()?;

    let (nats_tx, nats_rx) = bounded(100); // Channel for sending to NATS
    let (xone_tx, xone_rx) = bounded(100); // Channel for receiving from NATS

    std::thread::scope(|s| {
        s.spawn(move || {
            if let Err(e) = run_nats(xone_tx, nats_rx) {
                eprintln!("NATS error: {}", e);
            }
        });

        println!("Initializing XoneK2...");
        let k2 = XoneK2::new("XONE:K2", nats_tx, xone_rx);
        dbg!(&k2);

        std::thread::park();
    });

    Ok(())
}

fn list_midi_ports_jack() -> Result<(), Box<dyn Error>> {
    // Initialize JACK client
    let (client, _status) = Client::new("MidiPortLister", jack::ClientOptions::NO_START_SERVER)?;

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
