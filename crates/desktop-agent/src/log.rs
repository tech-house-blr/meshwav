//! Minimal leveled logging. Status lines go to stdout and problems to stderr (journald captures both);
//! the level comes from `--log-level` or `log_level` in the config file.
use std::sync::atomic::{AtomicU8, Ordering};

pub const ERROR: u8 = 0;
pub const WARN: u8 = 1;
pub const INFO: u8 = 2;
pub const DEBUG: u8 = 3;

static LEVEL: AtomicU8 = AtomicU8::new(INFO);

pub fn parse_level(s: &str) -> Option<u8> {
    match s.trim().to_ascii_lowercase().as_str() {
        "error" => Some(ERROR),
        "warn" | "warning" => Some(WARN),
        "info" => Some(INFO),
        "debug" => Some(DEBUG),
        _ => None,
    }
}

pub fn set_level(l: u8) {
    LEVEL.store(l, Ordering::Relaxed);
}

pub fn enabled(l: u8) -> bool {
    l <= LEVEL.load(Ordering::Relaxed)
}

macro_rules! error { ($($a:tt)*) => { if crate::log::enabled(crate::log::ERROR) { eprintln!($($a)*) } } }
macro_rules! warn { ($($a:tt)*) => { if crate::log::enabled(crate::log::WARN) { eprintln!($($a)*) } } }
macro_rules! info { ($($a:tt)*) => { if crate::log::enabled(crate::log::INFO) { println!($($a)*) } } }
macro_rules! debug { ($($a:tt)*) => { if crate::log::enabled(crate::log::DEBUG) { println!($($a)*) } } }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_parse_and_order() {
        assert_eq!(parse_level("Info"), Some(INFO));
        assert_eq!(parse_level(" debug "), Some(DEBUG));
        assert_eq!(parse_level("warning"), Some(WARN));
        assert_eq!(parse_level("loud"), None);
        assert!(ERROR < WARN && WARN < INFO && INFO < DEBUG);
    }
}
