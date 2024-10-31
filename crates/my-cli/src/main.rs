use midir::{Ignore, MidiInput, MidiOutput, MidiOutputConnection};
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;
mod xonek2;
use xonek2::*;

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

fn main() -> Result<(), Box<dyn Error>> {
    println!("Scanning for MIDI devices...");
    list_midi_ports()?;

    print!("run");
    let k2 = XoneK2::new("XONE:K2").unwrap();
    dbg!(&k2);
    k2.run()?;
    // Keep the application running
    std::thread::park();

    Ok(())
}
