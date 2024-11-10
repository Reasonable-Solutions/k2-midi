use std::{error::Error, thread::sleep, time::Duration};

use crossbeam_channel::{Receiver, Sender};
use jack::{
    Client, ClientOptions, Control, MidiIn, MidiOut, MidiWriter, Port, ProcessHandler, ProcessScope,
};

// This is midi in disguise carl, you can do better!
// (Turn these into _ONLY_ transport messages and add a separate mixer set later)
#[derive(Debug, Clone, Copy)]
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
    midi_in: Port<MidiIn>,
    midi_out: Port<MidiOut>,
}
impl ProcessHandler for XoneK2 {
    fn process(&mut self, _: &Client, ps: &ProcessScope) -> Control {
        let in_port = self.midi_in.iter(ps);

        let controls: Vec<_> = in_port
            .filter_map(|raw_midi| self.parse_midi_message(raw_midi.bytes))
            .collect();

        for control in controls {
            let _ = &mut self.handle_control(control, ps);
        }

        Control::Continue
    }
}

impl XoneK2 {
    pub fn new(
        device: &str,
        tx: Sender<XoneMessage>,
        rx: Receiver<XoneMessage>,
    ) -> Result<(), Box<dyn Error>> {
        let (client, _status) = Client::new("XoneK2-midi", ClientOptions::NO_START_SERVER)?;
        let midi_in = client.register_port("midi_in", MidiIn::default())?;
        let midi_out = client.register_port("midi_out", MidiOut::default())?;

        // Connect the ports first
        // TODO: Just fuzzy match on XONE:K2 capture/playback
        client.connect_ports_by_name(
            "Midi-Bridge:ALLEN-HEATH LTD- XONE:K2 at usb-0000:00:14-0-2- full speed:(capture_0) XONE:K2 MIDI 1",
            "XoneK2-midi:midi_in",
        )?;
        client.connect_ports_by_name(
            "XoneK2-midi:midi_out",
            "Midi-Bridge:ALLEN-HEATH LTD- XONE:K2 at usb-0000:00:14-0-2- full speed:(playback_0) XONE:K2 MIDI 1",
        )?;

        let xone = Self {
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
            midi_in,
            midi_out,
        };
        let _active_client = client.activate_async((), xone)?;
        std::thread::park();
        Ok(())
    }

    fn handle_control(&mut self, control: ControlType, ps: &ProcessScope) {
        let message = match control {
            ControlType::Encoder(id, direction) => &self.handle_encoder(id, direction),
            ControlType::Fader(id, level) => &self.handle_fader(id, level),
            ControlType::Knob(id, level) => &self.handle_knob(id, level),
            ControlType::Button(id, pressed) => &self.handle_button(id, pressed, ps),
        };
        dbg!(&control);
        if let Some(message) = message {
            let _ = self.tx.send(message.to_owned());
        }
    }

    fn parse_midi_message(&self, message: &[u8]) -> Option<ControlType> {
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
                let pressed = match (*status, *velocity) {
                    (NOTEON, 0x7f) => true,
                    (NOTEON, 0x00) => false, // Technically, this exists but i hate devices that do it
                    (NOTEOFF, _) => false,
                    _ => return None,
                };
                Some(ControlType::Button(*note, pressed))
            }

            _ => None,
        }
    }

    fn handle_encoder(&mut self, id: u8, direction: EncoderDirection) -> Option<XoneMessage> {
        match id {
            RENC if self.bottom_right_encoder_shift => match direction {
                EncoderDirection::Clockwise => {}
                EncoderDirection::CounterClockwise => {}
            },
            RENC => match direction {
                EncoderDirection::Clockwise => {}
                EncoderDirection::CounterClockwise => {}
            },
            LENC if self.bottom_left_encoder_shift => match direction {
                EncoderDirection::Clockwise => {}
                EncoderDirection::CounterClockwise => {}
            },
            LENC => match direction {
                EncoderDirection::Clockwise => {}
                EncoderDirection::CounterClockwise => {}
            },
            0x00..=0x03 => {}
            _ => return None,
        };

        Some(XoneMessage::Encoder {
            id: id,
            direction: direction,
        })
    }

    fn handle_fader(&mut self, id: u8, level: u8) -> Option<XoneMessage> {
        let normalized_level = level as f32 / 127.0;
        match id {
            0x10 => {} // println!("Fader 1: {:.2}", normalized_level),
            0x11 => {} //println!("Fader 2: {:.2}", normalized_level),
            0x12 => {} //println!("Fader 3: {:.2}", normalized_level),
            0x13 => {} // println!("Fader 4: {:.2}", normalized_level),
            _ => return None,
        }

        Some(XoneMessage::Fader {
            id: id,
            value: normalized_level,
        })
    }

    fn handle_knob(&mut self, id: u8, level: u8) -> Option<XoneMessage> {
        let normalized_level = level as f32 / 127.0;
        match id {
            0x04..=0x0f => {} // println!("Knob {}: {:.2}", id - 0x04, normalized_level),
            _ => return None,
        }

        Some(XoneMessage::Knob {
            id: id,
            value: normalized_level,
        })
    }

    fn handle_button(&mut self, id: u8, pressed: bool, ps: &ProcessScope) -> Option<XoneMessage> {
        match id {
            RENC => {
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
                        let _ = self.send_note_off(RSHIFT, ps);
                    } else {
                        let _ = self.send_note_with_color(RSHIFT, shift_to_color(&self.shift), ps);
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

        Some(XoneMessage::Button {
            id: id,
            pressed: pressed,
        })
    }

    fn send_note_with_color(
        &mut self,
        note: u8,
        color: Color,
        ps: &ProcessScope,
    ) -> Result<(), Box<dyn Error>> {
        let adjusted_note = apply_color(note, color);
        print!("{:#04x}", adjusted_note);
        let message = [0x9e, adjusted_note, 0x7F];
        let mut out_port = self.midi_out.writer(ps);
        out_port.write(&jack::RawMidi {
            time: 0,
            bytes: &message,
        })?;
        Ok(())
    }

    pub fn send_note_off(&mut self, note: u8, ps: &ProcessScope) -> Result<(), Box<dyn Error>> {
        let message = [0x8e, note, 0x00];
        let mut out_port = self.midi_out.writer(ps);
        out_port.write(&jack::RawMidi {
            time: 0,
            bytes: &message,
        })?;
        Ok(())
    }

    // These are trash, i cannot have sleep but i need to use the RawMidi{time: 0} etc
    pub fn send_note_off_all(&mut self, ps: &ProcessScope) -> Result<(), Box<dyn Error>> {
        let mut all_buttons: Vec<u8> = Vec::new();
        all_buttons.extend_from_slice(&TOPENCODERS);
        all_buttons.extend_from_slice(&POTBUTTONS);
        all_buttons.extend_from_slice(&BOTTOMBUTTONS);
        all_buttons.extend_from_slice(&[RSHIFT]);
        all_buttons.extend_from_slice(&[LSHIFT]);

        all_buttons.sort();
        for &button in &all_buttons {
            &self.send_note_off(button, ps)?;
            sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    pub fn send_note_color_all(&mut self, ps: &ProcessScope) -> Result<(), Box<dyn Error>> {
        let mut all_buttons: Vec<u8> = Vec::new();
        all_buttons.extend_from_slice(&TOPENCODERS);
        all_buttons.extend_from_slice(&POTBUTTONS);
        all_buttons.extend_from_slice(&BOTTOMBUTTONS);
        all_buttons.extend_from_slice(&[RSHIFT]);
        all_buttons.extend_from_slice(&[LSHIFT]);

        all_buttons.sort();

        for chunk in all_buttons.chunks(4) {
            for &button in chunk {
                if let Err(e) = &self.send_note_with_color(button, Color::Red, ps) {
                    eprintln!("Failed to send color for button {:#04x}: {}", button, e);
                }
            }
            sleep(Duration::from_millis(100));
            for &button in chunk {
                &self.send_note_off(button, ps)?;
            }
        }
        sleep(Duration::from_millis(1000));
        Ok(())
    }
}

#[derive(Debug)]
pub enum Color {
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
