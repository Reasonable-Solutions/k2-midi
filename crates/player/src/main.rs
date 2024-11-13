use claxon::FlacReader;
use crossbeam::channel::{bounded, Receiver, Sender};
use jack::{AudioOut, Client, ClientOptions, Control, ProcessScope};
use nats;
use rtrb::RingBuffer;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::{fs, thread};

static IS_PLAYING: AtomicBool = AtomicBool::new(true);

#[derive(Debug)]
enum PlayerCommand {
    Stop,
    Resume,
}

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
    let (mut producer, mut consumer) = RingBuffer::<(f32, f32)>::new(rtrb_buffer_size as usize);

    let (cmd_tx, cmd_rx) = bounded::<PlayerCommand>(32);

    let mut out_port_left = client
        .register_port("out_left", AudioOut::default())
        .expect("Failed to create left output port");
    let mut out_port_right = client
        .register_port("out_right", AudioOut::default())
        .expect("Failed to create right output port");

    let audio_file_path = PathBuf::from("./music/psy.flac");

    prefill_buffer(&audio_file_path, &mut producer, rtrb_buffer_size as usize);

    let decode_thread = thread::spawn(move || {
        decode_flac(
            audio_file_path,
            &mut producer,
            jack_sample_rate as u32,
            &cmd_rx,
            rtrb_buffer_size as usize,
        );
    });

    let cmd_tx_clone = cmd_tx.clone();
    thread::spawn(move || {
        control_thread(cmd_tx_clone);
    });

    let process_callback = move |_: &Client, ps: &ProcessScope| -> Control {
        let out_buffer_left = out_port_left.as_mut_slice(ps);
        let out_buffer_right = out_port_right.as_mut_slice(ps);

        if !IS_PLAYING.load(Ordering::Relaxed) {
            // If not playing, output silence
            for (left, right) in out_buffer_left.iter_mut().zip(out_buffer_right.iter_mut()) {
                *left = 0.0;
                *right = 0.0;
            }
            return Control::Continue;
        }

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

fn prefill_buffer(path: &PathBuf, producer: &mut rtrb::Producer<(f32, f32)>, buffer_size: usize) {
    let file = File::open(path).expect("Failed to open FLAC file");
    let mut reader = FlacReader::new(file).expect("Failed to create FLAC reader");
    let bits_per_sample = reader.streaminfo().bits_per_sample;
    let scale_factor = 1.0 / (1_i32 << (bits_per_sample - 1)) as f64;

    let mut left_sample = None;
    let mut samples_written = 0;

    for sample in reader.samples() {
        if samples_written >= buffer_size {
            break;
        }

        let sample = sample.expect("Failed to read sample") as f64 * scale_factor;
        let sample = sample.max(-1.0).min(1.0) as f32;

        match left_sample.take() {
            None => {
                left_sample = Some(sample);
            }
            Some(left) => {
                if producer.push((left, sample)).is_ok() {
                    samples_written += 1;
                }
            }
        }
    }
    println!("Buffer prefilled with {} samples", samples_written);
}
fn decode_flac(
    path: PathBuf,
    producer: &mut rtrb::Producer<(f32, f32)>,
    output_sample_rate: u32,
    cmd_rx: &Receiver<PlayerCommand>,
    skip_samples: usize,
) {
    let mut current_sample_index = skip_samples * 2; // stereo, so * 2

    loop {
        if IS_PLAYING.load(Ordering::Relaxed) {
            let file = File::open(&path).expect("Failed to open FLAC file");
            let mut reader = FlacReader::new(file).expect("Failed to create FLAC reader");
            let input_sample_rate = reader.streaminfo().sample_rate;
            let channels = reader.streaminfo().channels as usize;
            let bits_per_sample = reader.streaminfo().bits_per_sample;

            if channels != 2 {
                panic!("stereo please");
            }

            let scale_factor = 1.0 / (1_i32 << (bits_per_sample - 1)) as f64;

            let mut samples = reader.samples().skip(current_sample_index);
            let mut left_sample = None;

            for sample in samples {
                if let Ok(cmd) = cmd_rx.try_recv() {
                    match cmd {
                        PlayerCommand::Stop => {
                            println!("Stopping playback");
                            IS_PLAYING.store(false, Ordering::Relaxed);
                            break; // Break the sample loop but not the outer loop
                        }
                        PlayerCommand::Resume => {
                            println!("Resuming playback");
                            IS_PLAYING.store(true, Ordering::Relaxed);
                        }
                    }
                }

                let sample = sample.expect("Failed to read sample") as f64 * scale_factor;
                let sample = sample.max(-1.0).min(1.0) as f32;

                match left_sample.take() {
                    None => {
                        left_sample = Some(sample);
                        current_sample_index += 1;
                    }
                    Some(left) => {
                        while producer.push((left, sample)).is_err() {
                            thread::sleep(Duration::from_micros(10));

                            if let Ok(cmd) = cmd_rx.try_recv() {
                                match cmd {
                                    PlayerCommand::Stop => {
                                        println!("Stopping playback while waiting");
                                        IS_PLAYING.store(false, Ordering::Relaxed);
                                        break;
                                    }
                                    PlayerCommand::Resume => {
                                        println!("Resuming playback");
                                        IS_PLAYING.store(true, Ordering::Relaxed);
                                    }
                                }
                            }
                        }
                        current_sample_index += 1;
                    }
                }
            }
        }

        // When not playing, just check for commands
        match cmd_rx.recv_timeout(Duration::from_micros(100)) {
            Ok(PlayerCommand::Stop) => {
                println!("Already stopped");
                IS_PLAYING.store(false, Ordering::Relaxed);
            }
            Ok(PlayerCommand::Resume) => {
                println!("Resuming playback");
                IS_PLAYING.store(true, Ordering::Relaxed);
            }
            Err(_) => (), // Timeout is fine, just continue
        }
    }
}

fn control_thread(cmd_tx: Sender<PlayerCommand>) {
    let nc = nats::connect("nats://localhost:4222").expect("Failed to connect to NATS");

    let sub = nc
        .subscribe("xone.library.stop")
        .expect("Failed to subscribe to stop topic");

    println!("Control thread started, listening for NATS messages");

    let mut is_stopped = false;
    for msg in sub.messages() {
        let cmd = if is_stopped {
            is_stopped = false;
            println!("Received resume command via NATS");
            PlayerCommand::Resume
        } else {
            is_stopped = true;
            println!("Received stop command via NATS");
            PlayerCommand::Stop
        };

        if let Err(e) = cmd_tx.send(cmd) {
            eprintln!("Failed to send command: {}", e);
        }
    }
}
