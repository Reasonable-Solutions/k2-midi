use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

pub static ANAHATA_NO: AtomicU32 = AtomicU32::new(0);
pub static IS_PLAYING: AtomicBool = AtomicBool::new(true);
pub static CURRENT_POSITION: AtomicU64 = AtomicU64::new(0);
pub static DURATION: AtomicU64 = AtomicU64::new(0);
pub static PLAYHEAD: AtomicU64 = AtomicU64::new(0);
