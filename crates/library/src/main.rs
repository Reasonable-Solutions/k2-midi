use crossbeam::channel::{unbounded, Receiver, Sender};
use eframe::egui;
use eframe::egui::ColorImage;
use egui::{TextureHandle, TextureOptions};
use image::imageops::{self, FilterType};
use image::{load_from_memory, ImageReader};
use metaflac::{Block, Tag};
use nats::Connection;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::path::PathBuf;
use std::time::Duration;
use std::{fs, thread};

enum CounterMessage {
    Increment,
    Decrement,
    Select,
}

struct SelectMessage {
    file_path: String,
}

struct FlacFile {
    path: PathBuf,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    inline_album_art: Option<egui::TextureHandle>,
    large_album_art: Option<egui::TextureHandle>,
}

fn main() {
    let (ui_sender, ui_receiver): (Sender<CounterMessage>, Receiver<CounterMessage>) = unbounded();
    let (select_sender, select_receiver): (Sender<SelectMessage>, Receiver<SelectMessage>) =
        unbounded();

    let nats_client = nats::connect("nats://localhost:4222").unwrap();
    let nats_client_for_thread = nats_client.clone();

    thread::spawn(move || {
        for message in nats_client_for_thread
            .subscribe("xone.library")
            .unwrap()
            .messages()
        {
            dbg!("{:?}", String::from_utf8_lossy(&message.data));
            match message.data.as_slice() {
                b"Clockwise" => ui_sender.send(CounterMessage::Increment).unwrap(),
                b"CounterClockwise" => ui_sender.send(CounterMessage::Decrement).unwrap(),
                b"Select" => ui_sender.send(CounterMessage::Select).unwrap(),
                _ => (),
            }
        }
    });

    thread::spawn(move || {
        while let Ok(select_message) = select_receiver.recv() {
            nats_client
                .publish("xone.player.1.select", select_message.file_path.as_bytes())
                .unwrap();
        }
    });

    eframe::run_native(
        "FLAC File Selector",
        eframe::NativeOptions::default(),
        Box::new(|cc| {
            Ok(Box::new(FileSelectorApp::new(
                cc,
                ui_receiver,
                select_sender,
            )))
        }),
    );
}

struct FileSelectorApp {
    ui_receiver: Receiver<CounterMessage>,
    select_sender: Sender<SelectMessage>,
    flac_files: Vec<FlacFile>,
    selected_index: usize,
    album_art_cache: HashMap<u64, (TextureHandle, TextureHandle)>,
}

impl FileSelectorApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        ui_receiver: Receiver<CounterMessage>,
        select_sender: Sender<SelectMessage>,
    ) -> Self {
        let mut album_art_cache = HashMap::new();

        let mut flac_files: Vec<FlacFile> = fs::read_dir("./music")
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.extension().map(|ext| ext == "flac").unwrap_or(false) {
                    Some(FlacFile::from_path(&path, &cc, &mut album_art_cache))
                } else {
                    None
                }
            })
            .collect();

        Self {
            ui_receiver,
            select_sender,
            flac_files,
            selected_index: 0,
            album_art_cache,
        }
    }
}
impl eframe::App for FileSelectorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(message) = self.ui_receiver.try_recv() {
            match message {
                CounterMessage::Increment => {
                    if self.selected_index < self.flac_files.len() - 1 {
                        self.selected_index += 1;
                    }
                }
                CounterMessage::Decrement => {
                    if self.selected_index > 0 {
                        self.selected_index -= 1;
                    }
                }
                CounterMessage::Select => {
                    if let Some(selected_file) = self.flac_files.get(self.selected_index) {
                        let file_path = selected_file.path.to_string_lossy().to_string();
                        self.select_sender
                            .send(SelectMessage { file_path })
                            .unwrap();
                    }
                }
            }
        }
        ctx.request_repaint_after(Duration::from_millis(12));
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(selected_file) = self.flac_files.get(self.selected_index) {
                    if let Some(large_art) = &selected_file.large_album_art {
                        ui.image(large_art);
                    }
                }
            });

            for (i, file) in self.flac_files.iter().enumerate() {
                ui.horizontal(|ui| {
                    if let Some(inline_art) = &file.inline_album_art {
                        ui.image(inline_art);
                    }

                    let label = format!(
                        "{} - {}",
                        file.title
                            .clone()
                            .unwrap_or_else(|| "Unknown Title".to_string()),
                        file.artist
                            .clone()
                            .unwrap_or_else(|| "Unknown Artist".to_string())
                    );

                    if i == self.selected_index {
                        ui.colored_label(egui::Color32::YELLOW, label);
                    } else {
                        ui.label(label);
                    }
                });
            }
        });
    }
}

impl FlacFile {
    fn from_path(
        path: &PathBuf,
        cc: &eframe::CreationContext<'_>,
        cache: &mut HashMap<u64, (TextureHandle, TextureHandle)>,
    ) -> Self {
        let tag = Tag::read_from_path(path).ok();
        let (title, artist, album) = if let Some(tag) = &tag {
            (
                tag.get_vorbis("TITLE")
                    .and_then(|mut iter| iter.next().map(|s| s.to_string())),
                tag.get_vorbis("ARTIST")
                    .and_then(|mut iter| iter.next().map(|s| s.to_string())),
                tag.get_vorbis("ALBUM")
                    .and_then(|mut iter| iter.next().map(|s| s.to_string())),
            )
        } else {
            (None, None, None)
        };
        let (inline_album_art, large_album_art) = if let Some(tag) = tag {
            tag.blocks()
                .filter_map(|block| match block {
                    Block::Picture(pic) => {
                        if pic.picture_type == metaflac::block::PictureType::CoverFront {
                            Some(load_album_art(&pic.data, cc, cache))
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
                .next()
                .unwrap_or((None, None))
        } else {
            (None, None)
        };

        FlacFile {
            path: path.clone(),
            title,
            artist,
            album,
            inline_album_art,
            large_album_art,
        }
    }
}

fn load_album_art(
    data: &[u8],
    cc: &eframe::CreationContext<'_>,
    cache: &mut HashMap<u64, (TextureHandle, TextureHandle)>,
) -> (Option<TextureHandle>, Option<TextureHandle>) {
    let hash = calculate_hash(data);
    if let Some((inline_texture, large_texture)) = cache.get(&hash) {
        println!("Using cached album art for hash: {}", hash);
        return (Some(inline_texture.clone()), Some(large_texture.clone()));
    }
    // Cache miss
    let cursor = Cursor::new(data);
    let reader = ImageReader::new(cursor).with_guessed_format();
    let image = match reader {
        Ok(r) => match r.decode() {
            Ok(img) => img.into_rgba8(),
            Err(e) => {
                println!("Failed to decode image: {:?}", e);
                return (None, None);
            }
        },
        Err(e) => {
            println!("Failed to guess image format: {:?}", e);
            return (None, None);
        }
    };

    let square_thumbnail = image::imageops::resize(&image, 60, 60, FilterType::Lanczos3);
    let inline_thumbnail = image::imageops::crop_imm(
        &square_thumbnail,
        0,                                    // x offset
        (square_thumbnail.height() - 30) / 2, // y offset: center vertically
        60,                                   // width
        30,                                   // height
    )
    .to_image();

    let inline_color_image = ColorImage::from_rgba_unmultiplied(
        [
            inline_thumbnail.width() as usize,
            inline_thumbnail.height() as usize,
        ],
        &inline_thumbnail,
    );

    let inline_texture = cc.egui_ctx.load_texture(
        "inline_album_art",
        inline_color_image,
        TextureOptions::default(),
    );

    let large_thumbnail = image::imageops::resize(&image, 200, 200, FilterType::Lanczos3);
    let large_color_image = ColorImage::from_rgba_unmultiplied(
        [
            large_thumbnail.width() as usize,
            large_thumbnail.height() as usize,
        ],
        &large_thumbnail,
    );
    let large_texture = cc.egui_ctx.load_texture(
        "large_album_art",
        large_color_image,
        TextureOptions::default(),
    );

    cache.insert(hash, (inline_texture.clone(), large_texture.clone()));
    println!("Cached new album art for hash: {}", hash);

    (Some(inline_texture), Some(large_texture))
}

// Garbage should be a real full hash
fn calculate_hash(data: &[u8]) -> u64 {
    let hash_len = data.len().min(256);

    let mut hasher = DefaultHasher::new();
    hasher.write(&data[..hash_len]);
    hasher.finish()
}
