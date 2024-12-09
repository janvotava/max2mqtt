//! Firmware updates over HTTP (`POST /api/firmware`), into the app slot that is not running.
//!
//! The upload is an app image as written by `espflash save-image` (devenv: `fw-image`). It is
//! checked before anything is switched: ESP image magic, the chip it was built for, the size,
//! and its SHA-256 (espflash appends one). The bootloader checks the image again at boot.
//!
//! A new image is on trial until it has run for [`CONFIRM_AFTER`] and is connected to Wi-Fi
//! (or has no network configured). If it crashes [`MAX_TRIAL_BOOTS`] times, or is not
//! confirmed within [`TRIAL_LIMIT`], the previous image boots again. The default bootloader
//! does not roll back by itself, so the firmware counts trial boots in its own storage.

use alloc::format;
use alloc::string::String;
use core::sync::atomic::Ordering;

use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_bootloader_esp_idf::ota::OtaImageState;
use esp_bootloader_esp_idf::ota_updater::OtaUpdater;
use esp_bootloader_esp_idf::partitions::PARTITION_TABLE_MAX_LEN;
use sha2::{Digest, Sha256};

use crate::shared::STATION;
use crate::storage::{SharedStore, Store};

const SECTOR: usize = 4096;
const CONFIRM_AFTER: Duration = Duration::from_secs(60);
const TRIAL_LIMIT: Duration = Duration::from_secs(10 * 60);
const MAX_TRIAL_BOOTS: u8 = 3;

/// `chip_id` in the ESP image extended header.
#[cfg(feature = "c3-mini")]
const CHIP_ID: u16 = 0x0005;
#[cfg(feature = "d1-mini-esp32")]
const CHIP_ID: u16 = 0x0000;

const NO_OTA_TABLE: &str =
    "this board has the old partition table without update slots: flash it once over USB (fw-flash)";

fn updater<'a>(
    store: &'a mut Store,
    buf: &'a mut [u8; PARTITION_TABLE_MAX_LEN],
) -> Result<OtaUpdater<'a, 'static>, String> {
    OtaUpdater::new(store.flash(), buf).map_err(|_| String::from(NO_OTA_TABLE))
}

/// What the status page shows.
pub fn describe(store: &mut Store) -> (String, String) {
    let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];
    match updater(store, &mut buf) {
        Ok(mut u) => {
            let slot = u.selected_partition().map(|p| format!("{p:?}")).unwrap_or_else(|_| "?".into());
            let state = u.current_ota_state().map(|s| format!("{s:?}")).unwrap_or_else(|_| "?".into());
            (slot, state)
        }
        Err(_) => ("factory".into(), "no update slots".into()),
    }
}

/// Receive an image of `length` bytes, the first of which are `start`, write it to the
/// other slot and select it for the next boot.
pub async fn receive(
    socket: &mut TcpSocket<'_>,
    start: &[u8],
    length: usize,
    store: &SharedStore,
) -> Result<(), String> {
    let mut guard = store.lock().await;
    let store = guard.as_mut().ok_or("no flash storage")?;
    let mut table = [0u8; PARTITION_TABLE_MAX_LEN];
    let mut updater = updater(store, &mut table)?;
    let (mut region, slot) = updater.next_partition().map_err(|e| format!("no update slot: {e:?}"))?;
    if length > region.partition_size() {
        return Err(format!("image is {length} bytes, the slot holds {}", region.partition_size()));
    }
    if length < 64 {
        return Err("not a firmware image".into());
    }
    log::warn!("Firmware upload: {length} bytes into {slot:?}");

    let mut hasher = Sha256::new();
    let mut sector = alloc::vec![0u8; SECTOR];
    let mut filled = 0;
    let mut written = 0usize;
    let mut pending = start;
    let mut chunk = [0u8; 1024];
    let mut header_checked = false;
    // The last 32 bytes, if the image has an appended hash.
    let mut tail = [0u8; 32];
    let mut hash_appended = false;

    while written + filled < length {
        if pending.is_empty() {
            let n = with_timeout(Duration::from_secs(10), socket.read(&mut chunk))
                .await
                .map_err(|_| String::from("upload stalled"))?
                .map_err(|_| String::from("connection lost"))?;
            if n == 0 {
                return Err("upload incomplete".into());
            }
            pending = &chunk[..n];
        }
        let take = pending.len().min(SECTOR - filled).min(length - written - filled);
        let piece = &pending[..take];
        // Everything is hashed except an appended digest, which is kept to compare. (The
        // header, which says whether there is one, is long past by the end of the image.)
        let at = written + filled;
        let hash_end = if hash_appended { length - 32 } else { length };
        let hashed = hash_end.saturating_sub(at).min(take);
        hasher.update(&piece[..hashed]);
        for (i, b) in piece[hashed..].iter().enumerate() {
            tail[at + hashed + i - hash_end] = *b;
        }
        sector[filled..filled + take].copy_from_slice(piece);
        filled += take;
        pending = &pending[take..];

        if !header_checked && written + filled >= 24 {
            if sector[0] != 0xE9 {
                return Err("not an ESP app image (use the .bin from fw-image)".into());
            }
            let chip = u16::from_le_bytes([sector[12], sector[13]]);
            if chip != CHIP_ID {
                return Err(format!("image is for another chip (id {chip:#06x}, this board is {CHIP_ID:#06x})"));
            }
            hash_appended = sector[23] == 1;
            header_checked = true;
        }

        if filled == SECTOR || written + filled == length {
            // Pad the last sector to the flash word size.
            let padded = filled.next_multiple_of(4);
            sector[filled..padded].fill(0xFF);
            let offset = written as u32;
            region.erase(offset, offset + SECTOR as u32).map_err(|e| format!("erase failed: {e:?}"))?;
            region.write(offset, &sector[..padded]).map_err(|e| format!("write failed: {e:?}"))?;
            written += filled;
            filled = 0;
        }
    }

    if hash_appended && hasher.finalize().as_slice() != tail {
        return Err("checksum mismatch: the upload is corrupt".into());
    }
    updater.activate_next_partition().map_err(|e| format!("cannot select the new image: {e:?}"))?;
    updater.set_current_ota_state(OtaImageState::New).map_err(|e| format!("{e:?}"))?;
    log::warn!("Firmware written to {slot:?}, it boots next");
    Ok(())
}

/// At boot: if this image is on trial, count the boot, and roll back if it keeps crashing.
/// Returns whether the image needs confirming.
pub async fn check_boot(store: &mut Store) -> bool {
    let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];
    let Ok(state) = updater(store, &mut buf).map(|mut u| u.current_ota_state()) else { return false };
    if !matches!(state, Ok(OtaImageState::New | OtaImageState::PendingVerify)) {
        return false;
    }
    let boots = store.trial_boots().await.saturating_add(1);
    if boots > MAX_TRIAL_BOOTS {
        log::error!("The new firmware crashed {MAX_TRIAL_BOOTS} times, going back to the previous one");
        roll_back(store).await;
    }
    store.set_trial_boots(boots).await;
    log::warn!("New firmware on trial (boot {boots} of {MAX_TRIAL_BOOTS})");
    true
}

async fn roll_back(store: &mut Store) -> ! {
    store.set_trial_boots(0).await;
    let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];
    if let Ok(mut u) = updater(store, &mut buf) {
        let _ = u.set_current_ota_state(OtaImageState::Invalid);
        let _ = u.activate_next_partition();
        let _ = u.set_current_ota_state(OtaImageState::Valid);
    }
    Timer::after_millis(500).await;
    esp_hal::system::software_reset()
}

/// Confirm a new image once it has shown it works; roll back if it does not.
#[embassy_executor::task]
pub async fn confirm_task(store: &'static SharedStore, needs_network: bool) {
    loop {
        Timer::after_secs(5).await;
        let up = Instant::now().duration_since(Instant::from_ticks(0));
        let ok = up >= CONFIRM_AFTER && (!needs_network || STATION.load(Ordering::Relaxed));
        let mut guard = store.lock().await;
        let Some(store) = guard.as_mut() else { return };
        if ok {
            let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];
            if let Ok(mut u) = updater(store, &mut buf) {
                let _ = u.set_current_ota_state(OtaImageState::Valid);
            }
            store.set_trial_boots(0).await;
            log::info!("New firmware confirmed");
            return;
        }
        if up >= TRIAL_LIMIT {
            log::error!("The new firmware did not get onto the network, going back to the previous one");
            roll_back(store).await;
        }
    }
}
