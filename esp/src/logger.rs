//! Serial logger. Like esp-println's, but ends lines with CR LF so they also look right in
//! terminals that do not translate LF (`screen`, `picocom`, `minicom`), and prefixes the
//! local time (uptime until the clock is synced). The level comes from `ESP_LOG` at build time (`error` ... `trace`).
//!
//! The last [`BUFFER_BYTES`] of log also stay in RAM for the web UI (`GET /api/logs`), so
//! a board without USB can still be debugged.

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

use critical_section::Mutex;
use log::{Level, LevelFilter, Log, Metadata, Record};
use max2mqtt_bridge::logring::{Copied, LogRing};

const BUFFER_BYTES: usize = 8 * 1024;

static RING: Mutex<RefCell<LogRing>> = Mutex::new(RefCell::new(LogRing::new()));
/// Lines are kept once the heap exists.
static KEEP: AtomicBool = AtomicBool::new(false);

/// Start keeping lines; call after the heap is set up.
pub fn keep_lines() {
    critical_section::with(|cs| RING.borrow_ref_mut(cs).allocate(BUFFER_BYTES));
    KEEP.store(true, Ordering::Relaxed);
}

/// Lines with a sequence number from `after + 1` on, as many as fit in `max` bytes: see
/// [`LogRing::copy_after`].
pub fn copy_after(after: u32, out: &mut Vec<u8>, max: usize) -> Copied {
    critical_section::with(|cs| RING.borrow_ref(cs).copy_after(after, out, max))
}

const LEVEL: LevelFilter = match option_env!("ESP_LOG") {
    Some(s) => parse(s.as_bytes()),
    None => LevelFilter::Info,
};

const fn eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i].to_ascii_lowercase() != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

const fn parse(s: &[u8]) -> LevelFilter {
    if eq(s, b"off") {
        LevelFilter::Off
    } else if eq(s, b"error") {
        LevelFilter::Error
    } else if eq(s, b"warn") {
        LevelFilter::Warn
    } else if eq(s, b"debug") {
        LevelFilter::Debug
    } else if eq(s, b"trace") {
        LevelFilter::Trace
    } else {
        LevelFilter::Info
    }
}

struct Logger;

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= LEVEL
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let color = match record.level() {
            Level::Error => "\x1b[31m",
            Level::Warn => "\x1b[33m",
            Level::Info => "\x1b[32m",
            Level::Debug => "\x1b[34m",
            Level::Trace => "\x1b[35m",
        };
        // Local time once SNTP has answered, uptime before (from the hardware timer, which
        // runs before the embassy time driver does).
        let mut stamp: heapless::String<24> = heapless::String::new();
        match crate::shared::local_now() {
            Some((t, ms)) => {
                let _ = write!(
                    stamp,
                    "{}-{:02}-{:02} {:02}:{:02}:{:02}.{ms:03}",
                    t.year, t.month, t.day, t.hour, t.minute, t.second
                );
            }
            None => {
                let ms = esp_hal::time::Instant::now().duration_since_epoch().as_millis();
                let _ = write!(stamp, "{:>5}.{:03}", ms / 1000, ms % 1000);
            }
        }
        esp_println::print!("{color}{stamp} {:<5} {}\x1b[0m\r\n", record.level(), record.args());
        if KEEP.load(Ordering::Relaxed) {
            let mut line = String::new();
            let _ = write!(line, "{stamp} {:<5} {}", record.level(), record.args());
            critical_section::with(|cs| RING.borrow_ref_mut(cs).push(&line));
        }
    }

    fn flush(&self) {}
}

pub fn init() {
    static LOGGER: Logger = Logger;
    // SAFETY: called once at the start of main, before any other task or interrupt
    // handler that could log. The racy variants are needed on chips without atomics (C3).
    unsafe {
        let _ = log::set_logger_racy(&LOGGER);
        log::set_max_level_racy(LEVEL);
    }
}
