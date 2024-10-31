use midir::{Ignore, MidiInput, MidiOutput, MidiOutputConnection};
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;
mod xonek2;
use xonek2::*;

fn main() -> Result<(), Box<dyn Error>> {
    print!("run");
    let k2 = XoneK2::new("XONE:K2").unwrap();
    dbg!(&k2);
    k2.run();
    // Keep the application running
    Ok(())
}
