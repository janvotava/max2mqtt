//! State shared between tasks, and routing of the bridge's effects.

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex as Raw;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::Instant;
use max2mqtt_bridge::{Bridge, Ctx, Effect};

pub static BRIDGE: Mutex<Raw, Option<Bridge>> = Mutex::new(None);

/// Publish/subscribe effects for the MQTT task. Dropped while disconnected or full; the
/// bridge republishes everything after each connect.
pub static MQTT_OUT: Channel<Raw, Effect, 48> = Channel::new();

/// Something may be ready to transmit.
pub static TX_KICK: Signal<Raw, ()> = Signal::new();
pub static RADIO_REINIT: Signal<Raw, ()> = Signal::new();

/// Connected to the home network.
pub static STATION: AtomicBool = AtomicBool::new(false);
/// The setup hotspot is on.
pub static HOTSPOT: AtomicBool = AtomicBool::new(false);
pub static MQTT_CONNECTED: AtomicBool = AtomicBool::new(false);
/// A config save the bridge asked for did not happen (queue full, flash error): the
/// control loop tells the bridge, which asks again later.
pub static SAVE_FAILED: Signal<Raw, ()> = Signal::new();

/// Uptime in seconds of the last request from a hotspot client, 0 for never.
static HOTSPOT_ACTIVITY: AtomicU32 = AtomicU32::new(0);

pub fn hotspot_used() {
    let now = Instant::now().as_secs() as u32;
    HOTSPOT_ACTIVITY.store(now.max(1), Ordering::Relaxed);
}

/// Seconds since a hotspot client last did something.
pub fn hotspot_idle_secs() -> u64 {
    match HOTSPOT_ACTIVITY.load(Ordering::Relaxed) {
        0 => u64::MAX,
        t => Instant::now().as_secs().saturating_sub(t as u64),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    pub ssid: String,
    pub rssi: i8,
    pub secure: bool,
}

/// Start (`true`) or stop (`false`) measuring the CC1101's frequency error, see `radio`.
pub static CALIBRATE: Signal<Raw, bool> = Signal::new();

/// What the frequency measurement found so far.
#[derive(Debug, Clone, Default)]
pub struct Calibration {
    pub running: bool,
    /// FSCTRL0 the radio listens with right now, while searching.
    pub listening_at: i8,
    /// Per packet: sender, signal in dBm, the correction it implies.
    pub packets: Vec<(String, i16, i8)>,
    pub started_at: u64,
}

impl Calibration {
    /// The correction to use: the median over the packets so far.
    pub fn suggested(&self) -> Option<i8> {
        let mut v: Vec<i8> = self.packets.iter().map(|p| p.2).collect();
        v.sort_unstable();
        v.get(v.len() / 2).copied()
    }
}

pub static CALIBRATION: BlockingMutex<Raw, core::cell::RefCell<Calibration>> =
    BlockingMutex::new(core::cell::RefCell::new(Calibration {
        running: false,
        listening_at: 0,
        packets: Vec::new(),
        started_at: 0,
    }));

/// The web UI asks for a Wi-Fi scan; the Wi-Fi task answers on [`SCAN_RESULT`].
pub static SCAN_REQUEST: Signal<Raw, ()> = Signal::new();
pub static SCAN_RESULT: Signal<Raw, Result<Vec<Network>, ()>> = Signal::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemCmd {
    SaveConfig,
    /// Everything, settings too.
    FactoryReset,
    Restart,
}

/// Handled in order, so "erase, then restart" cannot be reordered.
pub static SYSTEM: Channel<Raw, SystemCmd, 8> = Channel::new();

/// (unix seconds, monotonic ms) at the last SNTP answer.
static CLOCK: BlockingMutex<Raw, Cell<Option<(u64, u64)>>> = BlockingMutex::new(Cell::new(None));

/// The bridge's time zone, for the logger (which cannot wait for the bridge).
static LOG_TIMEZONE: BlockingMutex<Raw, Cell<max2mqtt_bridge::clock::TimeZone>> =
    BlockingMutex::new(Cell::new(max2mqtt_bridge::clock::TimeZone { utc_offset_minutes: 60, eu_dst: true }));

pub fn set_log_timezone(tz: max2mqtt_bridge::clock::TimeZone) {
    LOG_TIMEZONE.lock(|c| c.set(tz));
}

/// Local wall-clock time and milliseconds, once SNTP has answered.
pub fn local_now() -> Option<(max2mqtt_bridge::clock::LocalTime, u64)> {
    let (unix, at) = CLOCK.lock(|c| c.get())?;
    let ms = unix * 1000 + Instant::now().as_millis().saturating_sub(at);
    let tz = LOG_TIMEZONE.lock(|c| c.get());
    Some((max2mqtt_bridge::clock::local_time(ms / 1000, &tz), ms % 1000))
}

pub fn set_unix_time(unix: u64) {
    let now = Instant::now().as_millis();
    CLOCK.lock(|c| c.set(Some((unix, now))));
}

pub fn ctx() -> Ctx {
    let now = Instant::now().as_millis();
    let unix_time = CLOCK.lock(|c| c.get()).map(|(unix, at)| unix + now.saturating_sub(at) / 1000);
    Ctx { now, unix_time }
}

pub async fn with_bridge<R>(f: impl FnOnce(&mut Bridge) -> R) -> R {
    let mut guard = BRIDGE.lock().await;
    f(guard.as_mut().expect("bridge is created before tasks start"))
}

pub fn dispatch(effects: Vec<Effect>) {
    for effect in effects {
        let cmd = match effect {
            Effect::Publish { .. } | Effect::Subscribe(_) | Effect::Unsubscribe(_) => {
                if MQTT_OUT.try_send(effect).is_err() {
                    log::debug!("MQTT queue full, dropping message");
                }
                continue;
            }
            Effect::ReinitRadio => {
                RADIO_REINIT.signal(());
                continue;
            }
            Effect::SaveConfig => SystemCmd::SaveConfig,
            Effect::Restart => SystemCmd::Restart,
        };
        if SYSTEM.try_send(cmd).is_err() {
            log::warn!("System queue full, dropping {cmd:?}");
            if cmd == SystemCmd::SaveConfig {
                SAVE_FAILED.signal(());
            }
        }
    }
    // Bridge calls may have queued radio frames.
    TX_KICK.signal(());
}
