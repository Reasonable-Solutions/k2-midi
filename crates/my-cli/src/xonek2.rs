use std::{
    error::Error,
    sync::{Arc, Mutex},
    thread::sleep,
    time::Duration,
};

use midir::{
    Ignore, MidiInput, MidiInputConnection, MidiInputPort, MidiOutput, MidiOutputConnection,
    MidiOutputPort,
};

#[derive(Debug, Clone, Copy)]
enum EncoderDirection {
    Clockwise,
    CounterClockwise,
}

#[derive(Debug, Clone, Copy)]
enum ControlType {
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
}

impl XoneK2 {
    pub fn new(device: &str) -> Result<Self, Box<dyn Error>> {
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
        })
    }
    pub fn run(mut self) -> Result<(), Box<dyn Error>> {
        print!("RUNNING");

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
        print!("MMMMMMMM");

        // Wrap self in Arc<Mutex> to share between threads
        let self_mutex = Arc::new(Mutex::new(self));
        let self_mutex_clone = Arc::clone(&self_mutex);

        let _conn_in = midi_in
            .connect(
                &in_port,
                "Xone K2 Input",
                move |_, message, _| {
                    // Debug print (optional)
                    println!(
                        "{:#04x},  {:#04x}, {:#04x}",
                        message[0], message[1], message[2]
                    );

                    // Lock mutex to access self
                    if let Ok(mut xone) = self_mutex_clone.lock() {
                        if let Some(control) = xone.parse_midi_message(message) {
                            xone.handle_control(control);
                        }
                    }
                },
                (),
            )
            .expect("Failed to connect to MIDI input");

        std::thread::park();
        Ok(())
    }
    fn handle_control(&mut self, control: ControlType) {
        match control {
            ControlType::Encoder(id, direction) => self.handle_encoder(id, direction),
            ControlType::Fader(id, level) => self.handle_fader(id, level),
            ControlType::Knob(id, level) => self.handle_knob(id, level),
            ControlType::Button(id, pressed) => self.handle_button(id, pressed),
        }
    }

    fn parse_midi_message(&mut self, message: &[u8]) -> Option<ControlType> {
        match message {
            // CC Messages (faders, knobs, encoder rotation)
            [0xbe, note, level] => match note {
                // Encoders rotation
                0x15 => Some(ControlType::Encoder(
                    RENC,
                    match level {
                        0x01 => EncoderDirection::Clockwise,
                        0x7f => EncoderDirection::CounterClockwise,
                        _ => return None,
                    },
                )),

                // Faders
                0x10..=0x13 => Some(ControlType::Fader(*note, *level)),

                // Knobs
                0x04..=0x09 => Some(ControlType::Knob(*note, *level)),

                _ => None,
            },

            // Note On/Off (buttons, encoder presses)
            [status @ (NOTEON | NOTEOFF), note, velocity] => {
                let pressed = *status == NOTEON && *velocity == 0x7f;
                match note {
                    0x0d => Some(ControlType::Button(*note, pressed)),
                    _ => None,
                }
            }

            _ => None,
        }
    }

    fn handle_encoder(&mut self, id: u8, direction: EncoderDirection) {
        match id {
            RENC => {
                if self.bottom_right_encoder_shift {
                    match direction {
                        EncoderDirection::Clockwise => todo!(),
                        EncoderDirection::CounterClockwise => todo!(),
                    }
                }
            }
            LENC => {
                if self.bottom_left_encoder_shift {
                    match direction {
                        EncoderDirection::Clockwise => todo!(),
                        EncoderDirection::CounterClockwise => todo!(),
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_fader(&mut self, id: u8, level: u8) {
        let normalized_level = level as f32 / 127.0;
        match id {
            0x10 => println!("Fader 1: {:.2}", normalized_level),
            0x11 => println!("Fader 2: {:.2}", normalized_level),
            0x12 => println!("Fader 3: {:.2}", normalized_level),
            0x13 => println!("Fader 4: {:.2}", normalized_level),
            _ => {}
        }
    }

    fn handle_knob(&mut self, id: u8, level: u8) {
        let normalized_level = level as f32 / 127.0;
        match id {
            0x04..=0x09 => println!("Knob {}: {:.2}", id - 0x04, normalized_level),
            _ => {}
        }
    }

    fn handle_button(&mut self, id: u8, pressed: bool) {
        match id {
            RENC => self.bottom_right_encoder_shift = pressed,
            LENC => self.bottom_left_encoder_shift = pressed,
            RSHIFT => {
                // Shift button pressed
                self.shift = self.shift.next();
                if self.shift == Shift::Off {
                    //  let _ = send_note_off(&mut conn_out, RSHIFT);
                } else {
                    let _ =
                   //     send_note_with_color(&mut conn_out, RSHIFT, shift_to_color(&self.shift));
                    println!("Shift activated, {:#?}", &self.shift);
                }
            }
            _ => {}
        }
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

pub fn send_note_with_color(
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
pub const NOTEOFF: u8 = 0x00;

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
