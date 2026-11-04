//! Minimal stderr logger. stdout is the MCP channel, so nothing here ever writes to it.
//! `IMPORTIN_LOG` = error | warn | info | debug | trace (case-insensitive), default warn.

use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error = 1,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    fn name(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        }
    }

    fn parse(s: &str) -> Option<Level> {
        [
            Level::Error,
            Level::Warn,
            Level::Info,
            Level::Debug,
            Level::Trace,
        ]
        .into_iter()
        .find(|l| s.trim().eq_ignore_ascii_case(l.name()))
    }
}

static MAX: AtomicU8 = AtomicU8::new(Level::Warn as u8);

/// Reads `IMPORTIN_LOG`; unset or unknown means warn.
pub fn init() {
    let level = std::env::var("IMPORTIN_LOG")
        .ok()
        .and_then(|v| Level::parse(&v))
        .unwrap_or(Level::Warn);
    MAX.store(level as u8, Ordering::Relaxed);
}

pub fn enabled(level: Level) -> bool {
    level as u8 <= MAX.load(Ordering::Relaxed)
}

/// `importin: <level>: <msg>` on stderr when `level` is enabled. Use the macros.
pub fn write(level: Level, msg: fmt::Arguments<'_>) {
    if enabled(level) {
        eprintln!("importin: {}: {msg}", level.name());
    }
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Warn, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Info, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Debug, format_args!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_levels_case_insensitively() {
        assert_eq!(Level::parse("DEBUG"), Some(Level::Debug));
        assert_eq!(Level::parse(" trace "), Some(Level::Trace));
        assert_eq!(Level::parse("error"), Some(Level::Error));
        assert_eq!(Level::parse("verbose"), None);
        assert_eq!(Level::parse(""), None);
    }

    #[test]
    fn levels_order_by_verbosity() {
        assert!(Level::Error < Level::Warn && Level::Debug < Level::Trace);
    }
}
