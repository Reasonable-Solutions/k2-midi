use std::{
    error::Error,
    sync::{Arc, Mutex},
    thread::sleep,
    time::Duration,
};

use crossbeam_channel::{bounded, Receiver, Sender};
use midir::{
    Ignore, MidiInput, MidiInputConnection, MidiInputPort, MidiOutput, MidiOutputConnection,
    MidiOutputPort,
};

// This is midi in disguise carl, you can do better!
#[derive(Debug, Clone)]
pub enum XoneMessage {
    Fader { id: u8, value: f32 },
    Encoder { id: u8, direction: EncoderDirection },
    Button { id: u8, pressed: bool },
    Knob { id: u8, value: f32 },
}

#[derive(Debug, Clone, Copy)]
pub enum EncoderDirection {
    Clockwise,
    CounterClockwise,
}

#[derive(Debug, Clone, Copy)]
pub enum ControlType {
    Encoder(u8, EncoderDirection), // (encoder_id, direction)
    Fader(u8, u8),                 // (fader_id, level)
    Knob(u8, u8),                  // (knob_id, level)
    Button(u8, bool),              // (button_id, pressed)
}

#[derive(Debug)]
pub struct XoneK2 {
    pub shift: Shift,
    pub bottom_left_encoder_shift: bool,
    pub bottom_right_encoder_shift: bool,
    pub top_left_encoder_shift: bool,
    pub top_mid_left_encoder_shift: bool,
    pub top_mid_right_encoder_shift: bool,
    pub top_right_encoder_shift: bool,
    pub device: String,
    pub tx: Sender<XoneMessage>,
    pub rx: Receiver<XoneMessage>,
}

impl XoneK2 {
    pub fn new(
        device: &str,
        tx: Sender<XoneMessage>,
        rx: Receiver<XoneMessage>,
    ) -> Result<Self, Box<dyn Error>> {
        println!("SETTING UP");
        Ok(Self {
            shift: Shift::Off,
            bottom_left_encoder_shift: false,
            bottom_right_encoder_shift: false,
            top_left_encoder_shift: false,
            top_mid_left_encoder_shift: false,
            top_mid_right_encoder_shift: false,
            top_right_encoder_shift: false,
            device: device.to_owned(),
            tx,
            rx,
        })
    }
    pub fn run(mut self) -> Result<(), Box<dyn Error>> {
        let mut midi_in = MidiInput::new("MIDI Input")?;
        midi_in.ignore(Ignore::None);

        let in_ports = midi_in.ports();
        let in_port = in_ports
            .iter()
            .find(|p| midi_in.port_name(p).unwrap().contains(&self.device))
            .ok_or("MIDI Input port not found")?
            .clone();

        let midi_out = MidiOutput::new("MIDI Output")?;
        let out_ports = midi_out.ports();
        let out_port = out_ports
            .iter()
            .find(|p| midi_out.port_name(p).unwrap().contains(&self.device))
            .ok_or("MIDI Output port not found")?;

        let mut conn_out = midi_out.connect(out_port, "Xone K2 Output")?;

        let _ = send_note_off_all(&mut conn_out);

        // Create a channel for communication between threads
        let (sender, receiver) = bounded(32); // Buffer size of 32 messages
        let sender_clone = sender.clone();

        // Spawn MIDI input handling thread
        let conn_in = midi_in.connect(
            &in_port,
            "Xone K2 Input",
            move |_, message, _| {
                println!(
                    "{:#04x}, {:#04x}, {:#04x}",
                    message[0], message[1], message[2]
                );
                let _ = sender_clone.send(message.to_vec());
            },
            (),
        )?;

        loop {
            match receiver.recv() {
                Ok(message) => {
                    if let Some(control) = self.parse_midi_message(&message) {
                        self.handle_control(&mut conn_out, control);
                    }
                }
                Err(e) => {
                    println!("Channel receive error: {}", e);
                    break;
                }
            }
        }

        Ok(())
    }
    fn handle_control(&mut self, out_conn: &mut MidiOutputConnection, control: ControlType) {
        let message = match control {
            ControlType::Encoder(id, direction) => self.handle_encoder(id, direction),
            ControlType::Fader(id, level) => self.handle_fader(id, level),
            ControlType::Knob(id, level) => self.handle_knob(id, level),
            ControlType::Button(id, pressed) => self.handle_button(out_conn, id, pressed),
        };

        if let Some(message) = message {
            if let Err(e) = self.tx.send(message) {
                eprintln!("Failed to send message: {}", e);
            }
        }
    }

    fn parse_midi_message(&mut self, message: &[u8]) -> Option<ControlType> {
        match message {
            // CC Messages (faders, knobs, encoder rotation)
            [0xbe, note, level] => match note {
                // Encoders rotation
                0x00..=0x03 => Some(ControlType::Encoder(
                    *note, // we know that it is an encoder, we just matched on it! ,
                    match level {
                        0x01 => EncoderDirection::Clockwise,
                        0x7f => EncoderDirection::CounterClockwise,
                        _ => return None,
                    },
                )),

                0x15 => Some(ControlType::Encoder(
                    RENC,
                    match level {
                        0x01 => EncoderDirection::Clockwise,
                        0x7f => EncoderDirection::CounterClockwise,
                        _ => return None,
                    },
                )),
                0x14 => Some(ControlType::Encoder(
                    LENC,
                    match level {
                        0x01 => EncoderDirection::Clockwise,
                        0x7f => EncoderDirection::CounterClockwise,
                        _ => return None,
                    },
                )),

                // Faders
                0x10..=0x13 => Some(ControlType::Fader(*note, *level)),

                // Knobs
                0x04..=0x0f => Some(ControlType::Knob(*note, *level)),

                _ => None,
            },

            // Note On/Off (buttons, encoder presses)
            [status @ (NOTEON | NOTEOFF), note, velocity] => {
                println!("status: {:#04x}", status);
                let pressed = match (*status, *velocity) {
                    (NOTEON, 0x7f) => true,
                    (NOTEON, 0x00) => false,
                    (NOTEOFF, _) => false,
                    _ => return None,
                };
                Some(ControlType::Button(*note, pressed))
            }

            [w, t, f] => {
                println!("status: {:#04x}, {:#04x}, {:#04x}", w, t, f);
                Some(ControlType::Button(*t, true))
            }

            _ => None,
        }
    }

    fn handle_encoder(&mut self, id: u8, direction: EncoderDirection) -> Option<XoneMessage> {
        let message = match id {
            RENC if self.bottom_right_encoder_shift => match direction {
                EncoderDirection::Clockwise => println!("S-CW"),
                EncoderDirection::CounterClockwise => println!("S-CCW"),
            },
            RENC => match direction {
                EncoderDirection::Clockwise => println!("CW"),
                EncoderDirection::CounterClockwise => println!("CCW"),
            },
            LENC if self.bottom_left_encoder_shift => match direction {
                EncoderDirection::Clockwise => println!("S-CW"),
                EncoderDirection::CounterClockwise => println!("S-CCW"),
            },
            LENC => match direction {
                EncoderDirection::Clockwise => println!("CW"),
                EncoderDirection::CounterClockwise => println!("CCW"),
            },
            _ => return None,
        };

        Some(XoneMessage::Encoder { id, direction })
    }

    fn handle_fader(&mut self, id: u8, level: u8) -> Option<XoneMessage> {
        let normalized_level = level as f32 / 127.0;
        match id {
            0x10 => println!("Fader 1: {:.2}", normalized_level),
            0x11 => println!("Fader 2: {:.2}", normalized_level),
            0x12 => println!("Fader 3: {:.2}", normalized_level),
            0x13 => println!("Fader 4: {:.2}", normalized_level),
            _ => return None,
        }

        Some(XoneMessage::Fader {
            id,
            value: normalized_level,
        })
    }

    fn handle_knob(&mut self, id: u8, level: u8) -> Option<XoneMessage> {
        let normalized_level = level as f32 / 127.0;
        match id {
            0x04..=0x0f => println!("Knob {}: {:.2}", id - 0x04, normalized_level),
            _ => return None,
        }

        Some(XoneMessage::Knob {
            id,
            value: normalized_level,
        })
    }

    fn handle_button(
        &mut self,
        conn_out: &mut MidiOutputConnection,
        id: u8,
        pressed: bool,
    ) -> Option<XoneMessage> {
        match id {
            RENC => {
                println!("press: {}", pressed);
                self.bottom_right_encoder_shift = pressed;
            }
            LENC => {
                self.bottom_left_encoder_shift = pressed;
            }
            RSHIFT => {
                // Shift button pressed
                if pressed {
                    self.shift = self.shift.next();
                    if self.shift == Shift::Off {
                        let _ = send_note_off(conn_out, RSHIFT);
                    } else {
                        let _ = send_note_with_color(conn_out, RSHIFT, shift_to_color(&self.shift));
                        println!("Shift activated, {:#?}", &self.shift);
                    }
                }
            }
            // rows of buttons, top encoders to bottom button field
            0x34..=0x37 => {}
            0x30..=0x33 => {}
            0x2c..=0x2f => {}
            0x28..=0x2b => {}
            0x24..=0x27 => {}
            0x20..=0x23 => {}
            0x1c..=0x1f => {}
            0x18..=0x1b => {}
            0x0c => {}
            _ => return None,
        }

        Some(XoneMessage::Button { id, pressed })
    }
}

#[derive(Debug)]
enum Color {
    Red,
    Amber,
    Green,
}

impl Color {
    pub fn offset(&self) -> u8 {
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

pub fn send_note_off(conn_out: &mut MidiOutputConnection, note: u8) -> Result<(), Box<dyn Error>> {
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

pub const TOPENCODERS: [u8; 4] = [0x34, 0x35, 0x36, 0x37];
pub const POTBUTTONS: [u8; 12] = [
    0x30, 0x31, 0x32, 0x33, 0x2c, 0x2d, 0x2e, 0x2f, 0x28, 0x29, 0x2a, 0x2b,
];
pub const BOTTOMBUTTONS: [u8; 16] = [
    0x24, 0x25, 0x26, 0x27, 0x20, 0x21, 0x22, 0x23, 0x1c, 0x1d, 0x1e, 0x1f, 0x18, 0x19, 0x1a, 0x1b,
];
pub const LSHIFT: u8 = 0x0c;
pub const RSHIFT: u8 = 0x0f;
pub const LENC: u8 = 0x0d;
pub const RENC: u8 = 0x0e;

pub const NOTEON: u8 = 0x9e;
pub const NOTEOFF: u8 = 0x8e;

pub fn shift_to_color(shift: &Shift) -> Color {
    match shift {
        Shift::Red => Color::Red,
        Shift::Amber => Color::Amber,
        Shift::Green => Color::Green,
        _ => Color::Red,
    }
}
impl Shift {
    pub fn next(&self) -> Shift {
        match self {
            Shift::Off => Shift::Red,
            Shift::Red => Shift::Amber,
            Shift::Amber => Shift::Green,
            Shift::Green => Shift::Off,
        }
    }
}

pub fn send_note_off_all(conn_out: &mut MidiOutputConnection) -> Result<(), Box<dyn Error>> {
    let mut all_buttons: Vec<u8> = Vec::new();
    all_buttons.extend_from_slice(&TOPENCODERS);
    all_buttons.extend_from_slice(&POTBUTTONS);
    all_buttons.extend_from_slice(&BOTTOMBUTTONS);
    all_buttons.extend_from_slice(&[RSHIFT]);
    all_buttons.extend_from_slice(&[LSHIFT]);

    for &button in &all_buttons {
        send_note_off(conn_out, button)?;
        sleep(Duration::from_millis(5));
    }
    Ok(())
}

pub fn send_note_color_all(conn_out: &mut MidiOutputConnection) -> Result<(), Box<dyn Error>> {
    let mut all_buttons: Vec<u8> = Vec::new();
    all_buttons.extend_from_slice(&TOPENCODERS);
    all_buttons.extend_from_slice(&POTBUTTONS);
    all_buttons.extend_from_slice(&BOTTOMBUTTONS);
    all_buttons.extend_from_slice(&[RSHIFT]);
    all_buttons.extend_from_slice(&[LSHIFT]);

    print!("{:#04x?}", all_buttons);
    for (i, button) in all_buttons.iter().enumerate() {
        print!("Button {:#04x} at index {}\n", button, i);

        // Send note with color and check for errors
        if let Err(e) = send_note_with_color(
            conn_out,
            *button,
            if button % 2 == 0 {
                Color::Amber
            } else {
                Color::Green
            },
        ) {
            eprintln!("Failed to send color for button {:#04x}: {}", button, e);
            return Err(e); // Optionally break or continue based on the error handling needed
        }

        // Slightly increase the delay to ensure the device can keep up
        sleep(Duration::from_millis(50));
    }

    Ok(())
}
