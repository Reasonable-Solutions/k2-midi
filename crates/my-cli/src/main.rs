use midir::{Ignore, MidiInput, MidiOutput, MidiOutputConnection};
use std::error::Error;
use std::sync::{Arc, Mutex};

// Struct to track shift state
struct ControllerState {
    shift: Shift,
}

pub struct Xonek2 {
    shift: Shift,
}

#[derive(Debug)]
enum Color {
    Red,
    Amber,
    Green,
}

impl Color {
    fn offset(&self) -> u8 {
        match self {
            Color::Red => 0,
            Color::Amber => 4,
            Color::Green => 8,
        }
    }
}

fn apply_color(note: u8, color: Color) -> u8 {
    note + color.offset()
}

fn send_note_with_color(
    conn_out: &mut MidiOutputConnection,
    note: u8,
    color: Color,
) -> Result<(), Box<dyn Error>> {
    let adjusted_note = apply_color(note, color);
    print!("{:#04x}", adjusted_note);
    let message = [0x9e, adjusted_note, 0x7F];

    conn_out.send(&message)?;
    Ok(())
}

fn send_note_off(conn_out: &mut MidiOutputConnection, note: u8) -> Result<(), Box<dyn Error>> {
    let message = [0x8e, note, 0x00];

    conn_out.send(&message)?;
    Ok(())
}

#[derive(PartialOrd, Ord, PartialEq, Eq, Debug)]
pub enum Shift {
    Off,
    Red,
    Amber,
    Green,
}

fn shift_to_color(shift: &Shift) -> Color {
    match shift {
        Shift::Red => Color::Red,
        Shift::Amber => Color::Amber,
        Shift::Green => Color::Green,
        _ => Color::Red,
    }
}
impl Shift {
    fn next(&self) -> Shift {
        match self {
            Shift::Off => Shift::Red,
            Shift::Red => Shift::Amber,
            Shift::Amber => Shift::Green,
            Shift::Green => Shift::Off,
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    // Set up MIDI input
    let mut midi_in = MidiInput::new("MIDI Input")?;
    midi_in.ignore(Ignore::None);
    let in_ports = midi_in.ports();
    let in_port = in_ports
        .iter()
        .find(|p| midi_in.port_name(p).unwrap().contains("XONE:K2"))
        .unwrap();

    // Set up MIDI output
    let mut midi_out = MidiOutput::new("MIDI Output")?;
    let out_ports = midi_out.ports();
    let out_port = out_ports
        .iter()
        .find(|p| midi_out.port_name(p).unwrap().contains("XONE:K2"))
        .unwrap();
    let mut conn_out = midi_out.connect(out_port, "Xone K2 Output")?;

    // Shared state for shift button tracking
    let controller_state = Arc::new(Mutex::new(ControllerState { shift: Shift::Off }));
    let state_clone = controller_state.clone();

    send_note_off(&mut conn_out, RSHIFT);
    // Connect to MIDI input and handle incoming messages
    let _conn_in = midi_in.connect(
        in_port,
        "Xone K2 Input",
        move |_, message, _| {
            let mut state = state_clone.lock().unwrap();

            match message {
                // Example Shift button Note On message (adjust values as needed for your controller)
                [NOTEON, RSHIFT, 0x7F] => {
                    // Shift button pressed
                    state.shift = state.shift.next();
                    if state.shift == Shift::Off {
                        let _ = send_note_off(&mut conn_out, RSHIFT);
                    } else {
                        let _ = send_note_with_color(
                            &mut conn_out,
                            RSHIFT,
                            shift_to_color(&state.shift),
                        );
                        println!("Shift activated, {:#?}", state.shift);
                    }
                }

                // Shift button Note Off message
                // Other buttons
                [NOTEON, note, velocity] => {
                    if state.shift == Shift::Amber {
                        println!(
                            "Shifted action for button: Note {:#04x?}, Velocity {:#04x?}",
                            note, velocity
                        );
                        // Handle shifted behavior here
                    } else {
                        // println!(
                        //     "Normal action for button: Note {:#04x?}, Velocity {:#04x?}",
                        //     note, velocity
                        // );
                        // Handle normal behavior here
                    }
                }
                [a, b, c] => {
                    println!("{:#04x},  {:#04x}, {:#04x}", a, b, c)
                }
                _ => {}
            }
        },
        (),
    )?;

    // Keep the application running
    std::thread::park();
    Ok(())
}

const TOPENCODERS: [u8; 4] = [0x34, 0x35, 0x36, 0x37];
const POTBUTTONS: [u8; 12] = [
    0x30, 0x31, 0x32, 0x33, 0x2c, 0x2d, 0x2e, 0x2f, 0x28, 0x29, 0x2a, 0x2b,
];
const BOTTOMBUTTONS: [u8; 16] = [
    0x24, 0x25, 0x26, 0x27, 0x20, 0x21, 0x22, 0x23, 0x1c, 0x1d, 0x1e, 0x1f, 0x18, 0x19, 0x1a, 0x1b,
];
const LSHIFT: u8 = 0x0c;
const RSHIFT: u8 = 0x0f;
const LENC: u8 = 0x0d;
const RENC: u8 = 0x0e;

const NOTEON: u8 = 0x9e;
const NOTEOFF: u8 = 0x00;

// // The Xone K2 uses different control numbers (second MIDI byte) to distinguish between
// // different colors for the LEDs. The baseline control number sets the LED to red. Adding
// // these offsets to the control number sets the LED to a different color.
// XoneK2.color = {
//     red: 0,
//     amber: 36,
//     green: 72
// };
// I need this, plus a function that takes an u8 and a color and gives a color the new value out in rust

// ```rust
// struct XoneK2 {
//     colors: [(u8, u8); 3],
// }

// impl XoneK2 {
//     fn get_color_with_offset(&self, control_number: u8, color: &str) -> Option<u8> {
//         match color {
//             "red" => Some(control_number + self.colors[0].0),
//             "amber" => Some(control_number + self.colors[1].0),
//             "green" => Some(control_number + self.colors[2].0),
//             _ => None,
//         }
//     }
// }
