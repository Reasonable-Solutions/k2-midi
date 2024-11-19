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

#[derive(Debug)]
enum UiMessage {
    Increment,
    Decrement,
    Select(u32),
}

struct SelectMessage {
    player: u32,
    file_path: String,
}

enum File {
    FlacFile(FlacFile),
    Dir(PathBuf),
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
    let (ui_sender, ui_receiver): (Sender<UiMessage>, Receiver<UiMessage>) = unbounded();
    let (select_sender, select_receiver): (Sender<SelectMessage>, Receiver<SelectMessage>) =
        unbounded();

    let nats_client = nats::connect("nats://localhost:4222").unwrap();
    let nats_client_for_thread = nats_client.clone();

    thread::spawn(move || {
        for message in nats_client_for_thread
            .subscribe("akasha.>")
            .unwrap()
            .messages()
        {
            dbg!(&message);
            // First check if it's a select message by looking at the subject
            if message.subject.ends_with(".select") {
                // Extract the number from the subject (akasha.1.select -> 1)
                if let Some(num) = message
                    .subject
                    .split('.')
                    .nth(1)
                    .and_then(|n| n.parse::<u32>().ok())
                {
                    ui_sender.send(UiMessage::Select(num)).unwrap();
                    continue;
                }
            }

            // Handle other messages as before
            match message.data.as_slice() {
                b"Clockwise" => ui_sender.send(UiMessage::Increment).unwrap(),
                b"CounterClockwise" => ui_sender.send(UiMessage::Decrement).unwrap(),
                _ => (),
            }
        }
    });

    thread::spawn(move || {
        while let Ok(select_message) = select_receiver.recv() {
            nats_client
                .publish(
                    &format!("anahata.{}.select", select_message.player),
                    select_message.file_path.as_bytes(),
                )
                .unwrap();
        }
    });

    eframe::run_native(
        "AKASHA",
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
    ui_receiver: Receiver<UiMessage>,
    select_sender: Sender<SelectMessage>,
    files: Vec<File>,
    selected_index: usize,
    current_dir: PathBuf,
    parent_exists: bool,
    album_art_cache: HashMap<u64, (TextureHandle, TextureHandle)>,
}

impl FileSelectorApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        ui_receiver: Receiver<UiMessage>,
        select_sender: Sender<SelectMessage>,
    ) -> Self {
        let mut album_art_cache = HashMap::new();
        let mut files: Vec<File> = fs::read_dir("./music")
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.extension().map(|ext| ext == "flac").unwrap_or(false) {
                    Some(File::FlacFile(FlacFile::from_path(
                        &path,
                        &cc.egui_ctx,
                        &mut album_art_cache,
                    )))
                } else if path.is_dir() {
                    Some(File::Dir(path))
                } else {
                    None
                }
            })
            .collect();

        Self {
            ui_receiver,
            select_sender,
            files,
            selected_index: 0,
            album_art_cache,
            parent_exists: false,
            current_dir: PathBuf::from("./music"),
        }
    }
    fn navigate_to(&mut self, path: PathBuf, ctx: &egui::Context) {
        let (files, cache) = self.load_directory(&path, ctx);
        self.current_dir = path;
        self.parent_exists = self.current_dir.parent().is_some();
        self.files = files;
        self.album_art_cache = cache;
        self.selected_index = 0;
    }

    fn load_directory(
        &self,
        dir: &PathBuf,
        ctx: &egui::Context,
    ) -> (Vec<File>, HashMap<u64, (TextureHandle, TextureHandle)>) {
        let mut album_art_cache = HashMap::new();
        let mut entries = Vec::new();

        if let Ok(read_dir) = fs::read_dir(dir) {
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    entries.push(File::Dir(path));
                } else if path.extension().map(|ext| ext == "flac").unwrap_or(false) {
                    entries.push(File::FlacFile(FlacFile::from_path(
                        &path,
                        ctx,
                        &mut album_art_cache,
                    )));
                }
            }
        }

        entries.sort_by(|a, b| match (a, b) {
            (File::Dir(_), File::FlacFile(_)) => std::cmp::Ordering::Less,
            (File::FlacFile(_), File::Dir(_)) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        });

        (entries, album_art_cache)
    }
}
impl eframe::App for FileSelectorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(message) = self.ui_receiver.try_recv() {
            match message {
                UiMessage::Increment => {
                    if self.selected_index < self.files.len() - 1 {
                        self.selected_index += 1;
                    }
                }
                UiMessage::Decrement => {
                    if self.selected_index > 0 {
                        self.selected_index -= 1;
                    }
                }
                UiMessage::Select(player) => {
                    if let Some(selected_file) = self.files.get(self.selected_index) {
                        match selected_file {
                            File::FlacFile(flac) => {
                                let file_path = flac.path.to_string_lossy().to_string();
                                if let Err(err) =
                                    self.select_sender.send(SelectMessage { player, file_path })
                                {
                                    eprintln!("Failed to send selection: {}", err);
                                }
                            }
                            File::Dir(path) => {
                                dbg!(&path);
                                self.navigate_to(path.clone(), ctx)
                            }
                        }
                    }
                }
            }
        }
        ctx.request_repaint_after(Duration::from_millis(12));

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.heading(
                    egui::RichText::new("AKASHA") //("आकाश") on hold because egui cant utf8 proper
                        .size(24.0)
                        .strong()
                        .color(egui::Color32::WHITE),
                );
            });

            ui.horizontal(|ui| {
                if let Some(file) = self.files.get(self.selected_index) {
                    match file {
                        File::FlacFile(flac) => {
                            if let Some(large_art) = &flac.large_album_art {
                                ui.image(large_art);
                            }
                        }
                        File::Dir(_) => (),
                    }
                }
            });

            for (i, file) in self.files.iter().enumerate() {
                ui.horizontal(|ui| match file {
                    File::FlacFile(flac) => {
                        if let Some(inline_art) = &flac.inline_album_art {
                            ui.image(inline_art);
                        }
                        let label = format!(
                            "🎵 {} - {}",
                            flac.title
                                .clone()
                                .unwrap_or_else(|| "Unknown Title".to_string()),
                            flac.artist
                                .clone()
                                .unwrap_or_else(|| "Unknown Artist".to_string())
                        );
                        if i == self.selected_index {
                            ui.colored_label(egui::Color32::YELLOW, label);
                        } else {
                            ui.label(label);
                        }
                    }
                    File::Dir(path) => {
                        let label = format!(
                            "📁 {}",
                            path.file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("Unknown")
                        );
                        if i == self.selected_index {
                            ui.colored_label(egui::Color32::YELLOW, label);
                        } else {
                            ui.label(label);
                        }
                    }
                });
            }
        });
    }
}

impl FlacFile {
    fn from_path(
        path: &PathBuf,
        cc: &egui::Context,
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
    cc: &egui::Context,
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

    let inline_texture = cc.load_texture(
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
    let large_texture = cc.load_texture(
        "large_album_art",
        large_color_image,
        TextureOptions::default(),
    );

    cache.insert(hash, (inline_texture.clone(), large_texture.clone()));
    println!("Cached new album art for hash: {}", hash);

    (Some(inline_texture), Some(large_texture))
}

// Garbage, should be a real full hash
fn calculate_hash(data: &[u8]) -> u64 {
    let hash_len = data.len().min(256);

    let mut hasher = DefaultHasher::new();
    hasher.write(&data[..hash_len]);
    hasher.finish()
}
