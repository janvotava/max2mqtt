//! Where the heap goes: its use after each startup step, and every new high after a web
//! request (with the request), in the log. For deciding what to make smaller.

use core::cell::Cell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

/// Not an atomic: the C3 has no atomic read-modify-write.
static PEAK: Mutex<CriticalSectionRawMutex, Cell<usize>> = Mutex::new(Cell::new(0));

fn kib(bytes: usize) -> usize {
    bytes.div_ceil(1024)
}

/// Startup: how much is in use after `step`.
pub fn step(step: &str) {
    let s = esp_alloc::HEAP.stats();
    log::info!("Heap after {step}: {} of {} KiB", kib(s.current_usage), kib(s.size));
}

/// After a request: logged when the heap in use is higher than after any request before.
pub fn note(what: &str) {
    let s = esp_alloc::HEAP.stats();
    let higher = PEAK.lock(|p| {
        let higher = s.current_usage > p.get();
        if higher {
            p.set(s.current_usage);
        }
        higher
    });
    if higher {
        log::info!(
            "Heap high {} KiB with {what} (highest ever {} of {} KiB)",
            kib(s.current_usage),
            kib(s.max_usage),
            kib(s.size)
        );
    }
}
