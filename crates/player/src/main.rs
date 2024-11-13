use claxon::FlacReader;
use jack::{AudioOut, Client, ClientOptions, Control, ProcessScope};
use rtrb::RingBuffer;
use std::fs::File;
use std::path::PathBuf;
use std::time::Duration;
use std::{fs, thread};

fn main() {
    let (client, _status) = Client::new("flac_player", ClientOptions::NO_START_SERVER)
        .expect("Failed to create JACK client");

    let jack_buffer_size = client.buffer_size();
    let jack_sample_rate = client.sample_rate();
    println!(
        "JACK buffer size: {}, sample rate: {}",
        jack_buffer_size, jack_sample_rate
    );

    let rtrb_buffer_size = jack_buffer_size * 32;
    // Left-right pairs
    let (mut producer, mut consumer) = RingBuffer::<(f32, f32)>::new(rtrb_buffer_size as usize);

    let mut out_port_left = client
        .register_port("out_left", AudioOut::default())
        .expect("Failed to create left output port");
    let mut out_port_right = client
        .register_port("out_right", AudioOut::default())
        .expect("Failed to create right output port");

    let audio_file_path = PathBuf::from("./music/psy.flac");
    thread::spawn(move || {
        decode_flac(audio_file_path, &mut producer, jack_sample_rate as u32);
    });

    let process_callback = move |_: &Client, ps: &ProcessScope| -> Control {
        let out_buffer_left = out_port_left.as_mut_slice(ps);
        let out_buffer_right = out_port_right.as_mut_slice(ps);

        for (left, right) in out_buffer_left.iter_mut().zip(out_buffer_right.iter_mut()) {
            if let Ok((l, r)) = consumer.pop() {
                *left = l;
                *right = r;
            } else {
                *left = 0.0;
                *right = 0.0;
            }
        }

        Control::Continue
    };

    let active_client = client
        .activate_async((), jack::ClosureProcessHandler::new(process_callback))
        .expect("Failed to activate client");

    thread::park();

    active_client
        .deactivate()
        .expect("Failed to deactivate client");
}

fn decode_flac(path: PathBuf, producer: &mut rtrb::Producer<(f32, f32)>, output_sample_rate: u32) {
    let file = File::open(&path).expect("Failed to open FLAC file");
    let mut reader = FlacReader::new(file).expect("Failed to create FLAC reader");
    let input_sample_rate = reader.streaminfo().sample_rate;
    let channels = reader.streaminfo().channels as usize;
    let bits_per_sample = reader.streaminfo().bits_per_sample;

    if channels != 2 {
        panic!("stereo please");
    }

    let scale_factor = 1.0 / (1_i32 << (bits_per_sample - 1)) as f64;

    println!(
        "FLAC: {} Hz, {} channels, {} bits | JACK: {} Hz",
        input_sample_rate, channels, bits_per_sample, output_sample_rate
    );

    let mut left_sample = None;

    for sample in reader.samples() {
        let sample = sample.expect("Failed to read sample") as f64 * scale_factor;
        let sample = sample.max(-1.0).min(1.0) as f32;

        match left_sample.take() {
            None => {
                left_sample = Some(sample);
            }
            Some(left) => {
                while producer.push((left, sample)).is_err() {
                    // I should check if skipping samples or blocking is better here
                    thread::sleep(Duration::from_micros(10));
                }
            }
        }
    }
}
