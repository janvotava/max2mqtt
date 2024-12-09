//! Crash records: what went wrong before the last restarts, kept for later.
//!
//! A panic prints itself on the serial port, then this module writes its message,
//! backtrace and time into RTC memory, which survives the reset that follows. At the next
//! boot the record moves to flash ([`crate::storage`], the last [`KEEP`]), so it also
//! survives a power cut. A watchdog reset (the firmware stopped running) leaves no message,
//! but the boot notices the reset reason and records that.
//!
//! `GET /api/crashes` lists them; `fw-crash` turns the backtrace addresses into source lines
//! with the ELF of that build.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::panic::PanicInfo;

use serde::{Deserialize, Serialize};

/// Crashes kept in flash.
pub const KEEP: usize = 5;
const MAGIC: u32 = 0x4352_5348; // "CRSH"
const MAX_FRAMES: usize = 16;
const MAX_MESSAGE: usize = 240;

/// Raw layout in RTC memory: magic, unix time (0: unknown), uptime in seconds, frame
/// count, frames, message length, message.
#[repr(C)]
#[derive(Clone, Copy)]
struct Slot {
    magic: u32,
    unix: u32,
    uptime: u32,
    frames: u32,
    pcs: [u32; MAX_FRAMES],
    len: u32,
    message: [u8; MAX_MESSAGE],
}

#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut SLOT: [u8; core::mem::size_of::<Slot>()] = [0; core::mem::size_of::<Slot>()];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Crash {
    /// Unix time, if the clock was synced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<u64>,
    pub uptime_seconds: u32,
    pub version: String,
    pub board: String,
    pub reason: String,
    /// Program counters, innermost first.
    pub backtrace: Vec<String>,
}

/// Bytes into a fixed buffer, cut off when full.
struct Truncating<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl Write for Truncating<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    esp_println::println!(
        "\r\n\x1b[31m====================== PANIC ======================\r\n{info}\x1b[0m\r\n\r\nBacktrace:\r\n"
    );
    let backtrace = esp_backtrace::Backtrace::capture();
    let mut slot = Slot {
        magic: MAGIC,
        unix: crate::shared::ctx().unix_time.unwrap_or(0) as u32,
        uptime: esp_hal::time::Instant::now().duration_since_epoch().as_secs() as u32,
        frames: 0,
        pcs: [0; MAX_FRAMES],
        len: 0,
        message: [0; MAX_MESSAGE],
    };
    for (k, frame) in backtrace.frames().iter().take(MAX_FRAMES).enumerate() {
        esp_println::println!("0x{:x}\r", frame.program_counter());
        slot.pcs[k] = frame.program_counter() as u32;
        slot.frames = k as u32 + 1;
    }
    let mut w = Truncating { buf: &mut slot.message, len: 0 };
    let _ = write!(w, "{info}");
    slot.len = w.len as u32;
    // SAFETY: only the panic handler writes, and nothing runs after it but the reset.
    unsafe {
        core::ptr::copy_nonoverlapping(
            (&raw const slot).cast::<u8>(),
            (&raw mut SLOT).cast::<u8>(),
            core::mem::size_of::<Slot>(),
        );
    }
    // Let the serial output drain, then restart: hanging would leave the board unreachable
    // with the relay as it was, possibly with the burner on. A reset turns it off.
    let start = esp_hal::time::Instant::now();
    while start.elapsed() < esp_hal::time::Duration::from_millis(500) {}
    esp_hal::system::software_reset()
}

/// What made the last restart worth recording, if anything. Clears the RTC record.
pub fn take() -> Option<Crash> {
    let version = String::from(env!("CARGO_PKG_VERSION"));
    let board = String::from(crate::board::NAME);
    // SAFETY: called once at boot, before anything could panic into the slot again; `Slot`
    // is plain old data, any bit pattern is a valid value.
    let slot: Slot = unsafe { core::ptr::read_unaligned((&raw const SLOT).cast::<Slot>()) };
    if slot.magic == MAGIC {
        // SAFETY: as above.
        unsafe { core::ptr::write_bytes((&raw mut SLOT).cast::<u8>(), 0, 4) };
        let len = (slot.len as usize).min(MAX_MESSAGE);
        let reason = String::from_utf8_lossy(&slot.message[..len]).into_owned();
        let frames = (slot.frames as usize).min(MAX_FRAMES);
        return Some(Crash {
            time: (slot.unix != 0).then_some(slot.unix as u64),
            uptime_seconds: slot.uptime,
            version,
            board,
            reason: alloc::format!("panic: {reason}"),
            backtrace: slot.pcs[..frames].iter().map(|pc| alloc::format!("0x{pc:08x}")).collect(),
        });
    }
    // No panic record: was it a watchdog?
    let reset = esp_hal::system::reset_reason()?;
    let name = alloc::format!("{reset:?}");
    name.contains("Wdt").then(|| Crash {
        time: None,
        uptime_seconds: 0,
        version,
        board,
        reason: alloc::format!("watchdog reset ({name}): the firmware stopped running"),
        backtrace: Vec::new(),
    })
}
