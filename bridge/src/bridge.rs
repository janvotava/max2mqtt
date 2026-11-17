//! The bridge: radio frames and MQTT messages in, radio frames and MQTT messages out.
//!
//! Hardware-free port of the old `main.cpp`. Each entry point takes a [`Ctx`] with the
//! current time and returns [`Effect`]s for the firmware to carry out.
//!
//! MQTT is for Home Assistant: state, and the commands its entities send. Everything else
//! is configured in the web UI ([`Bridge::configure_bridge`], [`Bridge::configure_device`]),
//! so whatever can publish to the broker cannot reconfigure or erase the bridge.
//! Topics (`max` is the default [`Topics::prefix`]):
//! - `max` (retained): bridge state; the will message sets it to offline.
//! - `max/<name>` (retained): state of one device.
//! - `max/<name>/set`: `temperature`, `mode`, `until`, see [`Bridge::handle_mqtt`].
//! - `max/set`: `{"pair": true}` or `false`.
//! - `max/reset`: restart.
//! - `max/init` (published): the radio was re-initialised after 5 minutes without traffic.
//! - `homeassistant/device/max2mqtt_<address>/config` (retained, optional): Home Assistant
//!   discovery, see [`crate::homeassistant`]. Republished when Home Assistant sends
//!   `online` on `homeassistant/status`.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::Millis;
use crate::address::{Address, hex};
use crate::clock::{LocalTime, TimeZone, Weekday, local_time};
use crate::config::{Config, DayProgram, DeviceConfig, HeatingConfig, Pending};
use crate::heating::{Burner, Demand, boost_active, valve_demand};
use crate::homeassistant::Discovery;
use crate::protocol::{
    DeviceType, FLAG_GROUP, Frame, Message, Mode, Status, TEMPERATURE_MAX, TEMPERATURE_MIN, ThermostatState,
    TxDescription, Until, WallThermostatState, command, decode_rx, parse, parse_thermostat_state,
    parse_wall_thermostat_state, payload,
};
use crate::schedule::{DaySchedule, parse_time};
use crate::tx::{Event, Next, Outgoing, TxId, TxQueue};

pub const OFFLINE_PAYLOAD: &str = r#"{"availability":"offline"}"#;

const MINUTE: Millis = 60_000;
/// Send a device's unconfirmed settings again at most this often (when it is heard).
const RESEND_EVERY: Millis = 15 * MINUTE;
/// Save this long after the first unsaved change, so a burst of changes is one flash write.
const SAVE_DELAY: Millis = 30_000;
/// Re-initialise the radio when nothing was received for this long.
const RADIO_WATCHDOG: Millis = 5 * MINUTE;
const TIME_SYNC_INTERVAL: Millis = 60 * MINUTE;
/// Each hour one sixth of the thermostats gets the time, so each one every 6 hours.
const TIME_SYNC_SLOTS: usize = 6;
/// A radiator valve that has not answered being asked is asked again after this.
const ASK_AGAIN: Millis = 30 * MINUTE;
/// Pairing switches itself off after this, so a stranger's device in pairing mode later
/// is not taken over.
pub const PAIRING_WINDOW: Millis = 5 * MINUTE;
/// Names that would collide with the bridge's own topics.
const RESERVED_NAMES: [&str; 6] = ["set", "rename", "save-config", "format", "reset", "init"];

/// Where the bridge publishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topics {
    /// First level of all bridge topics.
    pub prefix: String,
    /// Home Assistant discovery prefix, or `None` to not announce anything.
    pub discovery: Option<String>,
}

impl Default for Topics {
    fn default() -> Self {
        Topics { prefix: "max".into(), discovery: None }
    }
}

/// Time passed into every call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ctx {
    /// Monotonic milliseconds since boot.
    pub now: Millis,
    /// UTC seconds since 1970, once SNTP has answered.
    pub unix_time: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Publish {
        topic: String,
        payload: String,
        retain: bool,
    },
    Subscribe(String),
    Unsubscribe(String),
    /// Write [`Bridge::config`] to flash.
    SaveConfig,
    Restart,
    ReinitRadio,
}

/// A value and when it was last received.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timed<T> {
    pub value: T,
    pub at: Millis,
}

/// What we know about a device from the air. Not persisted.
#[derive(Clone, Debug, Default)]
pub struct Runtime {
    pub last_seen: Option<Millis>,
    pub rssi: Option<i16>,
    /// `at` is when the mode last *changed* (for the boost timer).
    pub mode: Option<Timed<Mode>>,
    pub desired: Option<Timed<f32>>,
    pub measured: Option<Timed<f32>>,
    pub valve: Option<Timed<u8>>,
    pub open: Option<bool>,
    pub rf_error: Option<bool>,
    pub low_battery: Option<bool>,
    /// What Home Assistant was last told about this device: (name, room, type).
    announced: Option<(String, String, DeviceType)>,
    /// When unconfirmed settings were last sent again.
    resent_at: Option<Millis>,
    /// `heating` as last published, to publish again when it changes.
    heating_sent: Option<bool>,
    /// When the device was last sent the time to get its state in the ACK (`ask_silent_valve`).
    asked_at: Option<Millis>,
    /// Stale partners this device has been told to drop since the bridge started.
    dropped_links: BTreeSet<Address>,
    /// The device was sent its group again after using another one, since start.
    group_fixed: bool,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub cfg: DeviceConfig,
    pub rt: Runtime,
}

impl Device {
    fn is_thermostat(&self) -> bool {
        self.cfg.device_type.is_some_and(DeviceType::is_thermostat)
    }

    fn is_heater(&self) -> bool {
        self.cfg.device_type.is_some_and(DeviceType::is_heater)
    }
}

pub struct Bridge {
    /// Everything but the devices, which live in `devices`.
    settings: Config,
    topics: Topics,
    devices: Vec<Device>,
    msgcnt: u8,
    /// Pairing is on until then.
    pairing_until: Option<Millis>,
    tx: TxQueue,
    burner: Burner,
    demand: Demand,
    booted_at: Option<String>,
    /// The latest time an entry point was called with, for state built outside one.
    now: Millis,
    last_rx: Millis,
    last_time_sync: Millis,
    time_sync_slot: usize,
    dirty_since: Option<Millis>,
    /// Devices still to publish after a (re)connect, one per [`Bridge::next_announcement`]:
    /// the next index, and whether to include their state.
    replay: Option<(usize, bool)>,
    /// A device's pending settings changed; the next tick schedules a save.
    pending_changed: bool,
}

/// One decimal, as f64 so JSON shows `21.2` and not `21.200000762939453`.
/// The setting a configuration command carries, to track until the device confirms it.
fn pending_item(cmd: u8, payload: &[u8]) -> Option<Pending> {
    Some(match cmd {
        command::SET_GROUP_ID | command::REMOVE_GROUP_ID => Pending::Group,
        command::ADD_LINK_PARTNER => Pending::Link(Address::from_slice(payload)?),
        command::REMOVE_LINK_PARTNER => {
            Pending::Unlinked(Address::from_slice(payload)?, DeviceType::from_raw(*payload.get(3)?)?)
        }
        command::CONFIG_TEMPERATURES => Pending::Temperatures,
        command::CONFIG_VALVE => Pending::Valve,
        command::SET_DISPLAY_ACTUAL_TEMPERATURE => Pending::Display,
        command::CONFIG_WEEK_PROFILE => Pending::Program(Weekday::from_max_index(payload.first()? & 0x07)?),
        _ => return None,
    })
}

/// The keys of `v` that MQTT may set; the others are logged.
fn only(topic: &str, v: &Value, allowed: &[&str]) -> Value {
    let Some(obj) = v.as_object() else { return Value::Null };
    let (keep, drop): (Map<String, Value>, Map<String, Value>) =
        obj.clone().into_iter().partition(|(k, _)| allowed.contains(&k.as_str()));
    if !drop.is_empty() {
        let keys: Vec<&str> = drop.keys().map(String::as_str).collect();
        log::warn!("{topic}: {keys:?} can only be changed in the web UI");
    }
    Value::Object(keep)
}

/// Window sensors and eco switches sleep, and only listen right after they sent something.
fn sleeps(t: Option<DeviceType>) -> bool {
    matches!(t, Some(DeviceType::ShutterContact | DeviceType::PushButton))
}

/// Devices a room links to each other.
fn linkable(t: Option<DeviceType>) -> bool {
    t.is_some_and(|t| t.is_thermostat() || t == DeviceType::ShutterContact)
}

/// Check a week program (`{"monday": {"6:00": 21.5, ...}, ...}`) before it goes anywhere,
/// so the web UI hears what is wrong.
pub fn validate_week(week: &Value) -> Result<(), String> {
    let days = week.as_object().ok_or("the week program must be an object of days")?;
    for (day, points) in days {
        let d = Weekday::parse(day).ok_or_else(|| format!("unknown day {day:?}"))?;
        let points = points.as_object().ok_or_else(|| format!("{}: expected an object of times", d.name()))?;
        let mut entries = Vec::new();
        for (time, t) in points {
            let minute = parse_time(time).map_err(|e| format!("{}: {e}", d.name()))?;
            let celsius = t.as_f64().ok_or_else(|| format!("{}: temperature at {time} is not a number", d.name()))?;
            entries.push((minute, celsius as f32));
        }
        DaySchedule::from_start_times(entries).map_err(|e| format!("{}: {e}", d.name()))?;
    }
    Ok(())
}

fn round1(x: f32) -> f64 {
    libm::round(x as f64 * 10.0) / 10.0
}

/// Overwrite the fields of `current` that `patch` names. Unknown keys are ignored.
fn merge<T: Serialize + DeserializeOwned>(current: &T, patch: &Value) -> Result<T, String> {
    let mut base = serde_json::to_value(current).map_err(|e| e.to_string())?;
    let (Some(base_obj), Some(patch_obj)) = (base.as_object_mut(), patch.as_object()) else {
        return Err("expected an object".into());
    };
    for (k, v) in patch_obj {
        if base_obj.contains_key(k) {
            base_obj.insert(k.clone(), v.clone());
        } else {
            log::warn!("Ignoring unknown setting {k:?}");
        }
    }
    serde_json::from_value(base).map_err(|e| e.to_string())
}

impl Bridge {
    pub fn new(mut config: Config, topics: Topics, now: Millis) -> Self {
        // A config from before delivery tracking: nobody knows what reached the devices, so
        // everything in a room counts as unconfirmed once.
        let migrate = !config.tracking;
        config.tracking = true;
        if let Err(e) = config.heating.validate() {
            log::warn!("Stored {e}; using the default heating settings");
            config.heating = HeatingConfig::default();
        }
        let devices = core::mem::take(&mut config.devices)
            .into_iter()
            .map(|cfg| Device { cfg, rt: Runtime::default() })
            .collect();
        let tx = TxQueue::new(config.duty_cycle_limit, now);
        let mut b = Bridge {
            settings: config,
            topics,
            devices,
            msgcnt: 0,
            pairing_until: None,
            tx,
            burner: Burner::default(),
            demand: Demand::default(),
            booted_at: None,
            now,
            last_rx: now,
            last_time_sync: now,
            time_sync_slot: 0,
            dirty_since: None,
            replay: None,
            pending_changed: false,
        };
        if migrate {
            for i in 0..b.devices.len() {
                b.mark_all_pending(i);
            }
            b.dirty_since = Some(now);
        }
        b
    }

    pub fn config(&self) -> Config {
        let mut c = self.settings.clone();
        c.devices = self.devices.iter().map(|d| d.cfg.clone()).collect();
        c
    }

    /// One device's stored settings, for the web UI's device page.
    pub fn device_config(&self, address: Address) -> Option<&DeviceConfig> {
        self.find(address).map(|i| &self.devices[i].cfg)
    }

    pub fn config_json(&self) -> String {
        serde_json::to_string(&self.config()).unwrap_or_default()
    }

    /// The config as JSON in pieces, for sending without holding all of it in memory:
    /// what comes before the device list, and after it. The devices go in between, comma
    /// separated, from [`Self::device_json`].
    pub fn config_json_around_devices(&self) -> (String, String) {
        // `settings` holds everything but the devices, which live in `self.devices`.
        let json = serde_json::to_string(&self.settings).unwrap_or_default();
        const LIST: &str = "\"devices\":[]";
        match json.find(LIST) {
            Some(at) => (json[..at + LIST.len() - 1].into(), json[at + LIST.len() - 1..].into()),
            None => (json, String::new()),
        }
    }

    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    /// Device `k`'s stored settings as JSON.
    pub fn device_json(&self, k: usize) -> Option<String> {
        self.devices.get(k).and_then(|d| serde_json::to_string(&d.cfg).ok())
    }

    pub fn topics(&self) -> &Topics {
        &self.topics
    }

    fn state_topic(&self, name: &str) -> String {
        format!("{}/{name}", self.topics.prefix)
    }

    fn set_topic(&self, name: &str) -> String {
        format!("{}/{name}/set", self.topics.prefix)
    }

    pub fn address(&self) -> Address {
        self.settings.address
    }

    pub fn timezone(&self) -> TimeZone {
        self.settings.timezone
    }

    pub fn devices(&self) -> &[Device] {
        &self.devices
    }

    /// Relay state the firmware should apply.
    pub fn burner_on(&self) -> bool {
        self.burner.on
    }

    fn local(&self, ctx: Ctx) -> Option<LocalTime> {
        ctx.unix_time.map(|t| local_time(t, &self.settings.timezone))
    }

    fn mark_dirty(&mut self, now: Millis) {
        self.dirty_since.get_or_insert(now);
    }

    /// The config save that [`Effect::SaveConfig`] asked for did not happen: try again later.
    pub fn save_failed(&mut self, now: Millis) {
        self.mark_dirty(now);
    }

    fn find(&self, address: Address) -> Option<usize> {
        self.devices.iter().position(|d| d.cfg.address == address)
    }

    fn find_by_name(&self, name: &str) -> Option<usize> {
        (!name.is_empty()).then(|| self.devices.iter().position(|d| d.cfg.name == name)).flatten()
    }

    // ----- sending -------------------------------------------------------------------

    fn next_msgcnt(&mut self) -> u8 {
        self.msgcnt = self.msgcnt.wrapping_add(1);
        self.msgcnt
    }

    fn frame(&mut self, dst: Address, flags: u8, group: u8, command: u8, payload: Vec<u8>) -> Frame {
        Frame { msgcnt: self.next_msgcnt(), flags, command, src: self.settings.address, dst, group, payload }
    }

    /// Queue a command for one device.
    fn send(&mut self, dst: Address, command: u8, payload: Vec<u8>) {
        if let Some(p) = pending_item(command, &payload)
            && let Some(i) = self.find(dst)
        {
            let pending = &mut self.devices[i].cfg.pending;
            // A newer link command replaces the opposite one.
            match p {
                Pending::Link(a) => {
                    pending.retain(|q| !matches!(q, Pending::Unlink(b) | Pending::Unlinked(b, _) if *b == a))
                }
                Pending::Unlink(a) | Pending::Unlinked(a, _) => {
                    pending.remove(&Pending::Link(a));
                    // Stored by an earlier firmware: replaced by the item with the type.
                    pending.remove(&Pending::Unlink(a));
                }
                _ => {}
            }
            self.pending_changed |= pending.insert(p);
        }
        let f = self.frame(dst, 0, 0, command, payload);
        let sleeps = self.find(dst).is_some_and(|i| sleeps(self.devices[i].cfg.device_type));
        if sleeps {
            self.tx.park(Outgoing::command(f));
        } else {
            self.tx.push(Outgoing::command(f));
        }
    }

    /// Queue a command with the device's group in the header (FHEM does this for
    /// SetTemperature): every device in the group takes it at once.
    fn send_grouped(&mut self, i: usize, command: u8, payload: Vec<u8>) {
        let (dst, group) = (self.devices[i].cfg.address, self.devices[i].cfg.group);
        let flags = if group != 0 { FLAG_GROUP } else { 0 };
        let f = self.frame(dst, flags, group, command, payload);
        self.tx.push(Outgoing::command(f));
    }

    fn ack_to(&mut self, dst: Address, msgcnt: u8) {
        let f = Frame {
            msgcnt,
            flags: 0,
            command: command::ACK,
            src: self.settings.address,
            dst,
            group: 0,
            payload: payload::ACK_OK.to_vec(),
        };
        self.tx.push_response(Outgoing::response(f));
    }

    fn send_time(&mut self, dst: Address, clock: &LocalTime) {
        // FHEM sends time with flags 04. The payload is refreshed right before sending.
        let f = self.frame(dst, FLAG_GROUP, 0, command::TIME_INFORMATION, payload::time_information(clock));
        self.tx.push(Outgoing::command(f));
    }

    /// What the radio should transmit next.
    pub fn next_tx(&mut self, ctx: Ctx) -> Next {
        if let Some(clock) = self.local(ctx)
            && let Some(f) = self.tx.head_frame_mut()
            && f.command == command::TIME_INFORMATION
            && f.payload.len() == 5
        {
            f.payload = payload::time_information(&clock);
        }
        let mut events = Vec::new();
        let next = self.tx.next(ctx.now, &mut events);
        self.log_events(events);
        if let Next::Send { bytes, long_preamble, .. } = &next {
            // Frame: length, counter, flags, command, source, destination, group, payload.
            let to = bytes.get(7..10).and_then(Address::from_slice);
            let to = match to.and_then(|a| self.find(a)) {
                Some(i) if !self.devices[i].cfg.name.is_empty() => self.devices[i].cfg.name.clone(),
                _ => to.map_or_else(String::new, |a| a.to_string()),
            };
            log::info!(
                "TX {to}{}: {}",
                if *long_preamble { " (long preamble)" } else { "" },
                TxDescription {
                    command: bytes.get(3).copied().unwrap_or(0xFF),
                    payload: bytes.get(crate::protocol::HEADER_LEN..).unwrap_or_default(),
                }
            );
            log::debug!("TX {to}: {}", hex(bytes));
        }
        next
    }

    pub fn transmitted(&mut self, id: TxId, now: Millis) {
        self.tx.transmitted(id, now);
    }

    fn log_events(&mut self, events: Vec<Event>) {
        for e in events {
            let name = |a: Address| self.find(a).map_or_else(|| a.to_string(), |i| self.devices[i].cfg.name.clone());
            match e {
                Event::Acked { dst, command, payload } => {
                    log::info!("{} acknowledged {}", name(dst), command::name(command));
                    self.confirm(dst, command, &payload);
                }
                Event::Nacked { dst, command, payload } => {
                    log::warn!("{} rejected {}: {payload:02X?}", name(dst), command::name(command));
                    // Removing a link the device does not have is rejected: nothing left to do.
                    // Anything else stays unconfirmed (shown in the UI) and goes out again with
                    // the other unconfirmed settings: some rejections pass, like during a boost.
                    if command == command::REMOVE_LINK_PARTNER {
                        self.confirm(dst, command, &payload);
                    }
                }
                Event::GaveUp { dst, command } => {
                    log::warn!(
                        "Gave up sending {} to {}, will retry when it is heard",
                        command::name(command),
                        name(dst)
                    )
                }
            }
        }
    }

    /// `dst` acknowledged a setting: it is no longer pending.
    fn confirm(&mut self, dst: Address, command: u8, payload: &[u8]) {
        let (Some(i), Some(p)) = (self.find(dst), pending_item(command, payload)) else { return };
        // The ACK of an older value does not confirm a newer one queued after it: with a
        // group changed twice in a row, the first ACK used to leave the device on the
        // first group for good.
        let current = match p {
            Pending::Group => payload == payload::set_group(self.devices[i].cfg.group).as_slice(),
            Pending::Temperatures => payload == self.temperatures_payload(i).as_slice(),
            Pending::Valve => payload == self.valve_payload(i).as_slice(),
            _ => true,
        };
        if !current {
            return;
        }
        // A week day in two messages is confirmed with its second one.
        if let Pending::Program(day) = p {
            let second = payload.first().is_some_and(|b| b & 0x10 != 0);
            if !second && self.program_parts(i, day) > 1 {
                return;
            }
        }
        self.pending_changed |= self.devices[i].cfg.pending.remove(&p);
    }

    fn program_parts(&self, i: usize, day: Weekday) -> usize {
        let Some(stored) = self.devices[i].cfg.schedule.get(&day) else { return 1 };
        DaySchedule::from_start_times(stored.entries()).map_or(1, |s| s.payloads(day.max_index()).len())
    }

    /// Everything a device in a room should have, as unconfirmed.
    fn mark_all_pending(&mut self, i: usize) {
        let d = &self.devices[i];
        if d.cfg.room.is_none() {
            return;
        }
        let mut p: Vec<Pending> = d.cfg.associations.iter().map(|&a| Pending::Link(a)).collect();
        if d.is_thermostat() {
            p.extend([Pending::Group, Pending::Temperatures, Pending::Valve]);
            p.extend(d.cfg.schedule.keys().map(|&day| Pending::Program(day)));
        }
        if d.cfg.display_actual_temperature.is_some() {
            p.push(Pending::Display);
        }
        self.devices[i].cfg.pending.extend(p);
        self.pending_changed = true;
    }

    /// Send a device's unconfirmed settings again, from the current config.
    fn send_pending(&mut self, i: usize, now: Millis) {
        self.devices[i].rt.resent_at = Some(now);
        let addr = self.devices[i].cfg.address;
        let items: Vec<Pending> = self.devices[i].cfg.pending.iter().copied().collect();
        log::info!("{}: sending {} unconfirmed settings again", self.devices[i].cfg.name, items.len());
        for p in items {
            match p {
                Pending::Group => {
                    let g = self.devices[i].cfg.group;
                    let cmd = if g == 0 { command::REMOVE_GROUP_ID } else { command::SET_GROUP_ID };
                    self.send(addr, cmd, payload::set_group(g));
                }
                Pending::Link(a) | Pending::Unlink(a) => {
                    let cmd = if matches!(p, Pending::Link(_)) {
                        command::ADD_LINK_PARTNER
                    } else {
                        command::REMOVE_LINK_PARTNER
                    };
                    match self.find(a).and_then(|j| self.devices[j].cfg.device_type) {
                        Some(t) => self.send(addr, cmd, payload::link_partner(a, t)),
                        // Partner gone or of unknown type: nothing to say about it.
                        None => _ = self.devices[i].cfg.pending.remove(&p),
                    }
                }
                Pending::Unlinked(a, t) => self.send(addr, command::REMOVE_LINK_PARTNER, payload::link_partner(a, t)),
                Pending::Temperatures => self.send_temperatures(i),
                Pending::Valve => self.send_valve(i),
                Pending::Display => match self.devices[i].cfg.display_actual_temperature {
                    Some(d) => {
                        self.send(addr, command::SET_DISPLAY_ACTUAL_TEMPERATURE, payload::display_actual_temperature(d))
                    }
                    None => _ = self.devices[i].cfg.pending.remove(&p),
                },
                Pending::Program(day) => {
                    if self.devices[i].cfg.schedule.contains_key(&day) {
                        self.send_schedule_day(i, day);
                    } else {
                        self.devices[i].cfg.pending.remove(&p);
                    }
                }
            }
        }
    }

    // ----- MQTT ----------------------------------------------------------------------

    /// Call after every (re)connect: subscribes and republishes state.
    pub fn on_mqtt_connected(&mut self) -> Vec<Effect> {
        let mut out = Vec::new();
        for t in ["reset", "set"] {
            out.push(Effect::Subscribe(format!("{}/{t}", self.topics.prefix)));
        }
        for d in &self.devices {
            if !d.cfg.name.is_empty() {
                out.push(Effect::Subscribe(self.set_topic(&d.cfg.name)));
            }
        }
        self.publish_bridge(&mut out);
        if let Some(status) = self.discovery().map(|d| d.status_topic()) {
            out.push(Effect::Subscribe(status));
        }
        self.announce_all(&mut out);
        self.replay = Some((0, true));
        out
    }

    /// After [`Self::on_mqtt_connected`] (or Home Assistant restarting): the next device's
    /// state and announcement, until `None`. One device at a time, so the firmware never
    /// holds all of them in memory: ten thermostats' announcements are tens of kilobytes.
    pub fn next_announcement(&mut self) -> Option<Vec<Effect>> {
        let (i, states) = self.replay?;
        if i >= self.devices.len() {
            self.replay = None;
            return None;
        }
        self.replay = Some((i + 1, states));
        let mut out = Vec::new();
        // Only devices heard since boot, so retained state is not overwritten with blanks.
        if states && self.devices[i].rt.last_seen.is_some() {
            self.publish_device(i, &mut out);
        }
        self.announce(i, &mut out);
        Some(out)
    }

    /// Handle a message on a subscribed topic.
    ///
    /// - `max/<name>/set`: `temperature` (number, `"eco"`, `"comfort"`, `"on"`, `"off"`; alias
    ///   `desired_temperature`), `mode` (`auto`/`manual`/`temporary`/`boost`), and `until`
    ///   (`"YYYY-MM-DD HH:MM"`, for temporary).
    /// - `max/set`: `pair`.
    /// - `max/reset`: restart. Retained messages are ignored, so a stray one cannot put the
    ///   bridge into a reboot loop.
    ///
    /// Other keys are configuration, which only the web UI can change; they are logged and
    /// ignored.
    pub fn handle_mqtt(&mut self, topic: &str, payload: &[u8], retained: bool, ctx: Ctx) -> Vec<Effect> {
        let mut out = Vec::new();
        if self.discovery().is_some_and(|d| d.status_topic() == topic) {
            // Home Assistant (re)started and forgot non-retained state; resend everything.
            if payload == b"online" {
                self.announce_all(&mut out);
            }
            return out;
        }
        let Some(rest) = topic.strip_prefix(self.topics.prefix.as_str()).and_then(|t| t.strip_prefix('/')) else {
            return out;
        };
        match rest {
            "reset" if retained => log::warn!("Ignoring retained message on {topic}"),
            "reset" => out.push(Effect::Restart),
            _ => {
                let value: Value = match serde_json::from_slice(payload) {
                    Ok(v) => v,
                    Err(e) => {
                        log::warn!("Cannot parse payload on {topic}: {e}");
                        return out;
                    }
                };
                match rest {
                    "set" => {
                        let control = only(topic, &value, &["pair"]);
                        self.set_bridge(&control, ctx, &mut out);
                    }
                    _ => match rest.strip_suffix("/set").map(|name| (name, self.find_by_name(name))) {
                        Some((_, Some(i))) => {
                            let control = only(topic, &value, &["temperature", "desired_temperature", "mode", "until"]);
                            self.set_device(i, &control, ctx);
                            self.device_changed(i, &mut out);
                        }
                        Some((name, None)) => log::warn!("No device named {name:?}"),
                        None => {}
                    },
                }
            }
        }
        out
    }

    fn validate_name(&self, name: &str, for_device: usize) -> Result<(), &'static str> {
        if name.is_empty() {
            return Err("name is empty");
        }
        if name.contains(['+', '#']) || name.starts_with('/') || name.ends_with('/') || name.contains("//") {
            return Err("name is not a valid topic level");
        }
        if RESERVED_NAMES.contains(&name) || name.rsplit('/').next() == Some("set") {
            return Err("name collides with a bridge topic");
        }
        if self.find_by_name(name).is_some_and(|j| j != for_device) {
            return Err("another device has this name");
        }
        Ok(())
    }

    fn rename_device(&mut self, i: usize, to: &str, ctx: Ctx, out: &mut Vec<Effect>) -> Result<(), &'static str> {
        let to = to.trim();
        if self.devices[i].cfg.name == to {
            return Ok(());
        }
        self.validate_name(to, i)?;
        let old = core::mem::replace(&mut self.devices[i].cfg.name, to.into());
        if !old.is_empty() {
            out.push(Effect::Unsubscribe(self.set_topic(&old)));
            out.push(Effect::Publish { topic: self.state_topic(&old), payload: String::new(), retain: true });
        }
        out.push(Effect::Subscribe(self.set_topic(to)));
        self.publish_device(i, out);
        self.announce(i, out);
        self.mark_dirty(ctx.now);
        Ok(())
    }

    fn set_bridge(&mut self, v: &Value, ctx: Ctx, out: &mut Vec<Effect>) {
        let Some(obj) = v.as_object() else { return };
        if let Some(a) = obj.get("address") {
            match a.as_str().and_then(|s| s.parse::<Address>().ok()) {
                Some(addr) if !addr.is_broadcast() => {
                    log::info!("Changing our address to {addr}");
                    self.settings.address = addr;
                    self.mark_dirty(ctx.now);
                }
                _ => log::warn!("Invalid address {a}"),
            }
        }
        if let Some(p) = obj.get("pair").and_then(Value::as_bool) {
            if p {
                log::info!("Pairing on for {} minutes", PAIRING_WINDOW / MINUTE);
                self.pairing_until = Some(ctx.now + PAIRING_WINDOW);
            } else {
                log::info!("Pairing off");
                self.pairing_until = None;
            }
        }
        if let Some(a) = obj.get("autocreate").and_then(Value::as_bool) {
            self.settings.autocreate = a;
            self.mark_dirty(ctx.now);
        }
        if let Some(d) = obj.get("duty_cycle_limit").and_then(Value::as_bool) {
            self.settings.duty_cycle_limit = d;
            self.tx.duty_cycle_limit = d;
            self.mark_dirty(ctx.now);
        }
        if let Some(h) = obj.get("heating") {
            match merge(&self.settings.heating, h).and_then(|h: HeatingConfig| h.validate().map(|()| h)) {
                Ok(heating) => {
                    self.settings.heating = heating;
                    self.mark_dirty(ctx.now);
                }
                Err(e) => log::warn!("Invalid heating settings: {e}"),
            }
        }
        if let Some(t) = obj.get("timezone") {
            match merge(&self.settings.timezone, t) {
                Ok(tz) => {
                    let changed = tz != self.settings.timezone;
                    self.settings.timezone = tz;
                    self.mark_dirty(ctx.now);
                    // Week programs run on the device's clock: correct it now, not over the
                    // next 6 hours of time sync slots.
                    if changed && let Some(clock) = self.local(ctx) {
                        let targets: Vec<Address> = self
                            .devices
                            .iter()
                            .filter(|d| d.is_thermostat() && d.cfg.room.is_some())
                            .map(|d| d.cfg.address)
                            .collect();
                        for addr in targets {
                            self.send_time(addr, &clock);
                        }
                    }
                }
                Err(e) => log::warn!("Invalid timezone: {e}"),
            }
        }
        self.publish_bridge(out);
    }

    /// A temperature given as a number or as one of `eco`, `comfort`, `on`, `off`.
    fn parse_temperature(&self, i: usize, v: &Value) -> Option<f32> {
        let t = match v {
            Value::Number(n) => n.as_f64()? as f32,
            Value::String(s) => match s.trim() {
                "eco" => self.devices[i].cfg.temperatures.eco,
                "comfort" => self.devices[i].cfg.temperatures.comfort,
                "on" => TEMPERATURE_MAX,
                "off" => TEMPERATURE_MIN,
                other => other.parse().ok()?,
            },
            _ => return None,
        };
        let t = libm::roundf(t * 2.0) / 2.0;
        (TEMPERATURE_MIN..=TEMPERATURE_MAX).contains(&t).then_some(t)
    }

    fn set_device(&mut self, i: usize, v: &Value, ctx: Ctx) {
        let Some(obj) = v.as_object() else {
            log::warn!("Device settings must be a JSON object");
            return;
        };
        let now = ctx.now;
        let name = self.devices[i].cfg.name.clone();
        let u8_field = |key: &str, max: u64| -> Option<u8> {
            let v = obj.get(key)?;
            match v.as_u64().filter(|&n| n <= max) {
                Some(n) => Some(n as u8),
                None => {
                    log::warn!("{name}: {key} must be 0-{max}, got {v}");
                    None
                }
            }
        };

        if let Some(r) = obj.get("room") {
            let room = r.as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
            if room != self.devices[i].cfg.room {
                log::info!("{name}: room {room:?}");
                let old = core::mem::replace(&mut self.devices[i].cfg.room, room.clone());
                self.mark_dirty(now);
                // The old room lets go of the device, the new one links it.
                if let Some(old) = old {
                    self.sync_room(&old, now);
                }
                self.sync_device(i, now);
                if let Some(room) = room {
                    self.sync_room(&room, now);
                }
            }
        }
        // For devices only heard in ACKs, which do not say what a device is.
        if let Some(t) = obj.get("type") {
            match serde_json::from_value::<DeviceType>(t.clone()) {
                Ok(t) => self.set_type(i, t, now),
                Err(_) => log::warn!("{name}: unknown device type {t}"),
            }
        }
        // Factory reset over the radio: the device forgets its pairing, links, group and
        // settings. Its entry here stays; holding boost on it pairs it again, and a device
        // with a room then gets everything back (see PairPing in `handle_radio`).
        if obj.get("factory_reset").and_then(Value::as_bool) == Some(true) {
            log::warn!("{name}: factory reset");
            self.send(self.devices[i].cfg.address, command::RESET, Vec::new());
        }
        // Remove a link the device still has to something the bridge no longer knows (a
        // forgotten window sensor whose unlink never arrived): it keeps trying to reach it
        // and reports a radio error. `{"unlink": {"address": "0FB714", "type": "shutter_contact"}}`.
        if let Some(u) = obj.get("unlink") {
            let address = u.get("address").and_then(Value::as_str).and_then(|a| a.parse::<Address>().ok());
            let partner_type = u.get("type").and_then(|t| serde_json::from_value::<DeviceType>(t.clone()).ok());
            match (address, partner_type) {
                (Some(a), Some(t)) => {
                    log::info!("{name}: removing its link to {a}");
                    self.send(self.devices[i].cfg.address, command::REMOVE_LINK_PARTNER, payload::link_partner(a, t));
                }
                _ => log::warn!("{name}: unlink needs an address and a type"),
            }
        }
        if obj.get("relink").and_then(Value::as_bool) == Some(true) {
            match self.devices[i].cfg.room.clone() {
                Some(room) => self.relink_room(&room, ctx),
                None => log::warn!("{name}: relink needs a room"),
            }
        }

        // Before anything that puts the group into the header.
        if let Some(group) = u8_field("group", 255) {
            self.send(self.devices[i].cfg.address, command::SET_GROUP_ID, payload::set_group(group));
            if self.devices[i].cfg.group != group {
                self.devices[i].cfg.group = group;
                self.mark_dirty(now);
            }
        }

        const TEMPERATURE_KEYS: [&str; 5] =
            ["comfort_temperature", "eco_temperature", "max_temperature", "min_temperature", "window_open_temperature"];
        if TEMPERATURE_KEYS.iter().any(|k| obj.contains_key(*k)) {
            let mut t = self.devices[i].cfg.temperatures.clone();
            for key in TEMPERATURE_KEYS {
                let Some(raw) = obj.get(key) else { continue };
                let Some(value) =
                    raw.as_f64().map(|x| x as f32).filter(|x| (TEMPERATURE_MIN..=TEMPERATURE_MAX).contains(x))
                else {
                    log::warn!("{name}: {key} must be {TEMPERATURE_MIN}-{TEMPERATURE_MAX}, got {raw}");
                    continue;
                };
                let field = match key {
                    "comfort_temperature" => &mut t.comfort,
                    "eco_temperature" => &mut t.eco,
                    "max_temperature" => &mut t.max,
                    "min_temperature" => &mut t.min,
                    _ => &mut t.window_open,
                };
                *field = value;
            }
            if t != self.devices[i].cfg.temperatures {
                self.devices[i].cfg.temperatures = t;
                self.mark_dirty(now);
            }
            self.send_temperatures(i);
        }

        const VALVE_KEYS: [&str; 6] = [
            "decalc_weekday",
            "decalc_hour",
            "boost_duration",
            "boost_valve_position",
            "max_valve_setting",
            "valve_offset",
        ];
        if VALVE_KEYS.iter().any(|k| obj.contains_key(*k)) {
            let mut vc = self.devices[i].cfg.valve.clone();
            if let Some(day) = obj.get("decalc_weekday") {
                match day.as_str().and_then(Weekday::parse) {
                    Some(d) => vc.decalc_day = d,
                    None => log::warn!("{name}: invalid decalc_weekday {day}"),
                }
            }
            if let Some(h) = u8_field("decalc_hour", 23) {
                vc.decalc_hour = h;
            }
            if let Some(m) = obj.get("boost_duration") {
                match m.as_u64() {
                    Some(m) => vc.boost_minutes = payload::boost_minutes_supported(m.min(u32::MAX as u64) as u32),
                    None => log::warn!("{name}: boost_duration must be minutes, got {m}"),
                }
            }
            if let Some(p) = u8_field("boost_valve_position", 100) {
                vc.boost_valve_position = p;
            }
            if let Some(p) = u8_field("max_valve_setting", 100) {
                vc.max_valve_setting = p;
            }
            if let Some(p) = u8_field("valve_offset", 100) {
                vc.valve_offset = p;
            }
            if vc != self.devices[i].cfg.valve {
                self.devices[i].cfg.valve = vc;
                self.mark_dirty(now);
            }
            self.send_valve(i);
        }

        if let Some(display) = obj.get("display_actual_temperature").and_then(Value::as_bool) {
            if self.devices[i].cfg.display_actual_temperature != Some(display) {
                self.devices[i].cfg.display_actual_temperature = Some(display);
                self.mark_dirty(now);
            }
            self.send(
                self.devices[i].cfg.address,
                command::SET_DISPLAY_ACTUAL_TEMPERATURE,
                payload::display_actual_temperature(display),
            );
        }

        if let Some(partner) = obj.get("associate").and_then(Value::as_str) {
            self.associate(i, partner, now);
        }

        if let Some(week) = obj.get("room_schedule") {
            self.set_room_schedule(i, week, now);
        }

        if let Some(schedule) = obj.get("schedule") {
            match obj.get("day").and_then(Value::as_str) {
                Some(day) => self.set_schedule_day(i, day, schedule, now),
                None => match schedule.as_object() {
                    Some(days) => {
                        for (day, s) in days {
                            self.set_schedule_day(i, day, s, now);
                        }
                    }
                    None => log::warn!("{name}: schedule must be an object"),
                },
            }
        }

        let temperature = obj.get("temperature").or_else(|| obj.get("desired_temperature"));
        if temperature.is_some() || obj.contains_key("mode") {
            self.set_desired(i, obj.get("mode"), temperature, obj.get("until"));
        }
    }

    /// The old firmware computed the mode as `device->mode || MODE_MANUAL`, a logical OR
    /// that is always 1, so every temperature change switched the device to manual. Now a
    /// device in auto stays in auto (a temporary override until the next switch point).
    /// Mode only, without temperature: auto and boost send 0 ("use the profile"); manual
    /// reuses the last known setpoint and is refused if none is known, where the old
    /// firmware sent -1 °C (garbage).
    fn set_desired(&mut self, i: usize, mode: Option<&Value>, temperature: Option<&Value>, until: Option<&Value>) {
        let name = self.devices[i].cfg.name.clone();
        let mode = match mode {
            Some(m) => match m.as_str().and_then(Mode::parse) {
                Some(m) => m,
                None => {
                    log::warn!("{name}: unknown mode {m}");
                    return;
                }
            },
            None => match self.devices[i].rt.mode {
                Some(Timed { value: Mode::Auto, .. }) => Mode::Auto,
                _ => Mode::Manual,
            },
        };
        let celsius = match temperature {
            Some(t) => match self.parse_temperature(i, t) {
                Some(c) => c,
                None => {
                    log::warn!("{name}: invalid temperature {t}");
                    return;
                }
            },
            None => match (mode, self.devices[i].rt.desired) {
                (Mode::Auto | Mode::Boost, _) => 0.0,
                (_, Some(d)) => d.value,
                (_, None) => {
                    log::warn!("{name}: no temperature given and none known");
                    return;
                }
            },
        };
        let until = if mode == Mode::Temporary {
            match until.and_then(Value::as_str).and_then(Until::parse) {
                Some(u) => Some(u),
                None => {
                    log::warn!("{name}: temporary mode needs \"until\": \"YYYY-MM-DD HH:MM\" (minutes 00 or 30)");
                    return;
                }
            }
        } else {
            None
        };
        log::info!("{name}: set {} {celsius}", mode.as_str());
        self.send_grouped(i, command::SET_TEMPERATURE, payload::set_temperature(mode, celsius, until));
    }

    fn set_schedule_day(&mut self, i: usize, day: &str, schedule: &Value, now: Millis) {
        let name = &self.devices[i].cfg.name;
        let Some(day) = Weekday::parse(day) else {
            // The old firmware wrote to schedule[255] here.
            log::warn!("{name}: unknown day {day:?}");
            return;
        };
        let Some(points) = schedule.as_object() else {
            log::warn!("{name}: schedule for {} must be an object", day.name());
            return;
        };
        let mut entries = Vec::new();
        for (time, temperature) in points {
            let minute = match parse_time(time) {
                Ok(m) => m,
                Err(e) => {
                    log::warn!("{name}: {e}");
                    return;
                }
            };
            let Some(celsius) = self.parse_temperature(i, temperature) else {
                log::warn!("{name}: invalid temperature {temperature} at {time}");
                return;
            };
            entries.push((minute, celsius));
        }
        if let Err(e) = DaySchedule::from_start_times(entries.iter().copied()) {
            log::warn!("{}: {}: {e}", self.devices[i].cfg.name, day.name());
            return;
        }
        self.devices[i].cfg.schedule.insert(day, DayProgram::new(entries));
        self.mark_dirty(now);
        self.send_schedule_day(i, day);
    }

    /// A week program for every thermostat in device `i`'s room (or just `i` without a
    /// room): each device runs its own program, links do not pass on switch points.
    fn set_room_schedule(&mut self, i: usize, week: &Value, now: Millis) {
        if let Err(e) = validate_week(week) {
            log::warn!("{}: {e}", self.devices[i].cfg.name);
            return;
        }
        let Some(days) = week.as_object() else { return };
        let room = self.devices[i].cfg.room.clone();
        let targets: Vec<usize> = match &room {
            Some(r) => (0..self.devices.len())
                .filter(|&j| self.devices[j].cfg.room.as_ref() == Some(r) && self.devices[j].is_thermostat())
                .collect(),
            None => alloc::vec![i],
        };
        for j in targets {
            for (day, points) in days {
                self.set_schedule_day(j, day, points, now);
            }
        }
    }

    fn send_schedule_day(&mut self, i: usize, day: Weekday) {
        let Some(stored) = self.devices[i].cfg.schedule.get(&day) else { return };
        match DaySchedule::from_start_times(stored.entries()) {
            Ok(s) => {
                let dst = self.devices[i].cfg.address;
                for p in s.payloads(day.max_index()) {
                    self.send(dst, command::CONFIG_WEEK_PROFILE, p);
                }
            }
            Err(e) => log::warn!("{}: stored schedule for {} is invalid: {e}", self.devices[i].cfg.name, day.name()),
        }
    }

    fn temperatures_payload(&self, i: usize) -> Vec<u8> {
        let t = &self.devices[i].cfg.temperatures;
        payload::config_temperatures(
            t.comfort,
            t.eco,
            t.max,
            t.min,
            t.measurement_offset,
            t.window_open,
            t.window_open_minutes,
        )
    }

    fn send_temperatures(&mut self, i: usize) {
        let p = self.temperatures_payload(i);
        self.send(self.devices[i].cfg.address, command::CONFIG_TEMPERATURES, p);
    }

    fn valve_payload(&self, i: usize) -> Vec<u8> {
        let v = &self.devices[i].cfg.valve;
        payload::config_valve(
            v.boost_minutes,
            v.boost_valve_position,
            v.decalc_day,
            v.decalc_hour,
            v.max_valve_setting,
            v.valve_offset,
        )
    }

    fn send_valve(&mut self, i: usize) {
        let p = self.valve_payload(i);
        self.send(self.devices[i].cfg.address, command::CONFIG_VALVE, p);
    }

    /// Tell `a` about `b` and `b` about `a`. Needs the type of the partner being announced.
    fn link(&mut self, a: usize, b: usize) {
        let (addr_a, type_a) = (self.devices[a].cfg.address, self.devices[a].cfg.device_type);
        let (addr_b, type_b) = (self.devices[b].cfg.address, self.devices[b].cfg.device_type);
        if let Some(tb) = type_b {
            self.send(addr_a, command::ADD_LINK_PARTNER, payload::link_partner(addr_b, tb));
        }
        if let Some(ta) = type_a {
            self.send(addr_b, command::ADD_LINK_PARTNER, payload::link_partner(addr_a, ta));
        }
    }

    fn associate(&mut self, i: usize, partner: &str, now: Millis) {
        let Some(j) = self.find_by_name(partner).filter(|&j| j != i) else {
            log::warn!("{}: cannot associate with {partner:?}: no such other device", self.devices[i].cfg.name);
            return;
        };
        self.link(i, j);
        for (from, to) in [(i, j), (j, i)] {
            let addr = self.devices[to].cfg.address;
            if !self.devices[from].cfg.associations.contains(&addr) {
                self.devices[from].cfg.associations.push(addr);
                self.mark_dirty(now);
            }
        }
    }

    /// After pairing or a factory reset: restore what we know about the device.
    fn send_configuration(&mut self, i: usize, ctx: Ctx) {
        log::info!("Restoring configuration of {}", self.devices[i].cfg.name);
        let addr = self.devices[i].cfg.address;
        // Window sensors and eco switches have no clock.
        if self.devices[i].is_thermostat()
            && let Some(clock) = self.local(ctx)
        {
            self.send_time(addr, &clock);
        }
        let group = self.devices[i].cfg.group;
        if group != 0 {
            self.send(addr, command::SET_GROUP_ID, payload::set_group(group));
        }
        if let Some(display) = self.devices[i].cfg.display_actual_temperature {
            self.send(addr, command::SET_DISPLAY_ACTUAL_TEMPERATURE, payload::display_actual_temperature(display));
        }
        if self.devices[i].cfg.device_type.is_none_or(DeviceType::is_thermostat) {
            self.send_valve(i);
            self.send_temperatures(i);
        }
        let partners: Vec<usize> = self.devices[i].cfg.associations.iter().filter_map(|&a| self.find(a)).collect();
        for j in partners {
            self.link(i, j);
        }
        let days: Vec<Weekday> = self.devices[i].cfg.schedule.keys().copied().collect();
        for day in days {
            self.send_schedule_day(i, day);
        }
    }

    // ----- radio ---------------------------------------------------------------------

    /// Handle the bytes read from the CC1101 RX FIFO.
    pub fn handle_radio(&mut self, raw: &[u8], ctx: Ctx) -> Vec<Effect> {
        let mut out = Vec::new();
        let now = ctx.now;
        self.now = now;
        if self.pairing_until.is_some_and(|t| now >= t) {
            self.pairing_until = None;
        }
        let rx = match decode_rx(raw) {
            Ok(rx) => rx,
            Err(e) => {
                log::debug!("Dropping frame {}: {e:?}", hex(raw));
                return out;
            }
        };
        self.last_rx = now;
        let f = rx.frame;
        let me = self.settings.address;
        if f.src == me {
            return out;
        }
        let to_me = f.dst == me;
        let msg = parse(&f);
        let who = |a: Address| match self.find(a) {
            Some(i) if !self.devices[i].cfg.name.is_empty() => self.devices[i].cfg.name.clone(),
            _ if a == me => "bridge".into(),
            _ if a.is_broadcast() => "all".into(),
            _ => a.to_string(),
        };
        let sender_type = self.find(f.src).and_then(|i| self.devices[i].cfg.device_type);
        log::info!(
            "RX {} -> {} #{}{} {} dBm: {}",
            who(f.src),
            who(f.dst),
            f.msgcnt,
            // The group the sender puts in its header: what it holds, whatever we sent it.
            if f.group != 0 { format!(" g{}", f.group) } else { String::new() },
            rx.rssi_dbm,
            match msg {
                // Not decoded, but maybe known by name (wake up, reset...).
                Message::Unknown => format!("{} ({:#04x}, {:02X?})", command::name(f.command), f.command, f.payload),
                Message::Malformed =>
                    format!("{} (command {:#04x}, {:02X?})", msg.describe(sender_type), f.command, f.payload),
                _ => msg.describe(sender_type),
            }
        );

        let (i, is_new) = match self.find(f.src) {
            Some(i) => (i, false),
            None => {
                if !(self.settings.autocreate || self.pairing_until.is_some()) {
                    return out;
                }
                let name = f.src.to_string();
                out.push(Effect::Subscribe(self.set_topic(&name)));
                self.devices.push(Device {
                    cfg: DeviceConfig { address: f.src, name, ..Default::default() },
                    rt: Runtime::default(),
                });
                self.mark_dirty(now);
                (self.devices.len() - 1, true)
            }
        };
        self.devices[i].rt.last_seen = Some(now);
        self.devices[i].rt.rssi = Some(rx.rssi_dbm);
        self.drop_stale_link(i, &f, &msg);
        self.fix_group(i, &f);

        match msg {
            Message::PairPing { device_type, .. } => {
                if let Some(t) = device_type {
                    self.set_type(i, t, now);
                }
                let managed = !is_new && self.devices[i].cfg.room.is_some();
                if !f.dst.is_broadcast() && !to_me {
                    // Paired to another cube. The old firmware answered these when pairing
                    // was on, and then pushed our whole configuration to the device.
                    log::info!("{} is paired to {}, not to us", f.src, f.dst);
                } else if to_me || self.pairing_until.is_some() || managed {
                    let pong = self.frame(f.src, 0, 0, command::PAIR_PONG, payload::PAIR_PONG.to_vec());
                    self.tx.push_front(Outgoing::command(pong));
                    if !to_me {
                        // New pairing or after a factory reset; a re-pairing keeps its settings.
                        self.send_configuration(i, ctx);
                    }
                }
            }
            Message::Ack { ok, state } => {
                if to_me {
                    let mut events = Vec::new();
                    self.tx.ack(f.src, f.msgcnt, ok, &mut events);
                    self.log_events(events);
                }
                if ok {
                    self.apply_ack_state(i, &state, now);
                }
            }
            Message::TimeRequest if to_me => match self.local(ctx) {
                Some(clock) => {
                    let reply = Frame {
                        msgcnt: f.msgcnt,
                        flags: FLAG_GROUP,
                        command: command::TIME_INFORMATION,
                        src: me,
                        dst: f.src,
                        group: f.group,
                        payload: payload::time_information(&clock),
                    };
                    self.tx.push_response(Outgoing::response(reply));
                }
                None => log::warn!("{} asked for the time, but it is not known yet", f.src),
            },
            Message::ThermostatState(s) => {
                if !self.devices[i].is_heater() {
                    self.set_type(i, DeviceType::HeatingThermostat, now);
                }
                self.apply_thermostat(i, &s, now);
            }
            Message::WallThermostatState(s) => {
                if self.devices[i].cfg.device_type.is_none() {
                    self.set_type(i, DeviceType::WallThermostat, now);
                }
                self.apply_wall_thermostat(i, &s, now);
            }
            Message::WallThermostatControl { desired, measured } => {
                self.set_type(i, DeviceType::WallThermostat, now);
                self.set_desired_reading(i, desired, now);
                self.devices[i].rt.measured = Some(Timed { value: measured, at: now });
            }
            Message::SetTemperature { mode, desired } => {
                self.set_mode(i, mode, now);
                self.set_desired_reading(i, desired, now);
                if to_me {
                    self.ack_to(f.src, f.msgcnt);
                }
            }
            Message::ShutterContactState { open, status } => {
                self.set_type(i, DeviceType::ShutterContact, now);
                self.devices[i].rt.open = Some(open);
                self.apply_status(i, status, false, now);
                if to_me {
                    self.ack_to(f.src, f.msgcnt);
                }
            }
            Message::PushButtonState { status } => {
                self.set_type(i, DeviceType::PushButton, now);
                self.apply_status(i, status, false, now);
                if to_me {
                    self.ack_to(f.src, f.msgcnt);
                }
            }
            Message::TimeRequest | Message::TimeInformation | Message::Unknown | Message::Malformed => {}
        }

        // Unconfirmed settings go again when the device is around and nothing else is
        // queued for it (at most every 15 minutes, for a device that keeps not answering).
        let d = &self.devices[i];
        if d.cfg.room.is_some()
            && !d.cfg.pending.is_empty()
            && !self.tx.has_for(f.src)
            && d.rt.resent_at.is_none_or(|t| now.saturating_sub(t) >= RESEND_EVERY)
        {
            self.send_pending(i, now);
        }
        // A sleeping device listens right after it sent something.
        self.tx.wake(f.src);
        self.publish_device(i, &mut out);
        self.announce(i, &mut out);
        out
    }

    fn set_type(&mut self, i: usize, t: DeviceType, now: Millis) {
        if self.devices[i].cfg.device_type != Some(t) {
            log::info!("{} is a {}", self.devices[i].cfg.name, t.as_str());
            self.devices[i].cfg.device_type = Some(t);
            self.mark_dirty(now);
            // Links need the type, so a room set before we knew it is linked now.
            if let Some(room) = self.devices[i].cfg.room.clone() {
                self.sync_room(&room, now);
            }
        }
    }

    // ----- rooms ---------------------------------------------------------------------
    //
    // Like a MAX! Cube: the thermostats in a room share a group id, and the thermostats and
    // window sensors in a room are linked to each other, both ways. Linked to a wall
    // thermostat, a radiator thermostat follows its setpoint and regulates on its
    // temperature; linked to a window sensor, it drops to the window-open temperature.

    /// The group id of `room`: the one its devices have, or the lowest one no other room uses.
    fn room_group(&self, room: &str, except: usize) -> u8 {
        let in_room = |d: &Device| d.cfg.room.as_deref() == Some(room);
        if let Some(g) = self
            .devices
            .iter()
            .enumerate()
            .filter(|&(j, d)| j != except && in_room(d) && d.is_thermostat() && d.cfg.group != 0)
            .map(|(_, d)| d.cfg.group)
            .min()
        {
            return g;
        }
        let used: BTreeSet<u8> = self
            .devices
            .iter()
            .enumerate()
            .filter(|&(j, d)| j != except && d.cfg.room.is_some())
            .map(|(_, d)| d.cfg.group)
            .collect();
        (1..=255).find(|g| !used.contains(g)).unwrap_or(0)
    }

    /// Bring one device's group id and links in line with its room.
    fn sync_device(&mut self, i: usize, now: Millis) {
        let d = &self.devices[i];
        let room = d.cfg.room.clone();
        let group = match &room {
            Some(r) if d.is_thermostat() => self.room_group(r, i),
            _ => 0,
        };
        let partners: Vec<(Address, DeviceType)> = match &room {
            Some(r) if linkable(d.cfg.device_type) => self
                .devices
                .iter()
                .enumerate()
                .filter(|&(j, o)| j != i && o.cfg.room.as_ref() == Some(r) && linkable(o.cfg.device_type))
                // Two window sensors have nothing to say to each other.
                .filter(|&(_, o)| !(sleeps(o.cfg.device_type) && sleeps(d.cfg.device_type)))
                .filter_map(|(_, o)| o.cfg.device_type.map(|t| (o.cfg.address, t)))
                .collect(),
            _ => Vec::new(),
        };
        let addr = d.cfg.address;
        let name = d.cfg.name.clone();

        if self.devices[i].cfg.group != group {
            log::info!("{name}: group {group}");
            if group == 0 {
                self.send(addr, command::REMOVE_GROUP_ID, payload::set_group(0));
            } else {
                self.send(addr, command::SET_GROUP_ID, payload::set_group(group));
            }
            self.devices[i].cfg.group = group;
            self.mark_dirty(now);
        }

        // A thermostat new to the room takes over the room's week program.
        if self.devices[i].is_thermostat()
            && self.devices[i].cfg.schedule.is_empty()
            && let Some(r) = &room
            && let Some(week) = self
                .devices
                .iter()
                .find(|o| o.cfg.room.as_ref() == Some(r) && o.is_thermostat() && !o.cfg.schedule.is_empty())
                .map(|o| o.cfg.schedule.clone())
        {
            log::info!("{name}: week program of room {r}");
            self.devices[i].cfg.schedule = week;
            self.mark_dirty(now);
            let days: Vec<Weekday> = self.devices[i].cfg.schedule.keys().copied().collect();
            for day in days {
                self.send_schedule_day(i, day);
            }
        }

        let current = self.devices[i].cfg.associations.clone();
        for &(p, t) in &partners {
            if !current.contains(&p) {
                log::info!("{name}: link to {p}");
                self.send(addr, command::ADD_LINK_PARTNER, payload::link_partner(p, t));
                self.devices[i].cfg.associations.push(p);
                self.mark_dirty(now);
            }
        }
        for p in current {
            if !partners.iter().any(|&(a, _)| a == p) {
                log::info!("{name}: unlink {p}");
                // The partner's type is part of the message; without it, just forget the link.
                if let Some(t) = self.find(p).and_then(|j| self.devices[j].cfg.device_type) {
                    self.send(addr, command::REMOVE_LINK_PARTNER, payload::link_partner(p, t));
                }
                self.devices[i].cfg.associations.retain(|&a| a != p);
                self.mark_dirty(now);
            }
        }
    }

    fn sync_room(&mut self, room: &str, now: Millis) {
        for i in 0..self.devices.len() {
            if self.devices[i].cfg.room.as_deref() == Some(room) {
                self.sync_device(i, now);
            }
        }
    }

    /// Send everything a room's devices should have again: group id, links, temperatures,
    /// valve settings, display and week program. For a device that was reset or replaced,
    /// and for anything still queued when the bridge restarted (the queue is not stored).
    fn relink_room(&mut self, room: &str, ctx: Ctx) {
        log::info!("Sending room {room} its setup again");
        self.sync_room(room, ctx.now);
        for i in 0..self.devices.len() {
            if self.devices[i].cfg.room.as_deref() == Some(room) {
                self.send_configuration(i, ctx);
            }
        }
    }

    fn set_mode(&mut self, i: usize, mode: Mode, now: Millis) {
        let rt = &mut self.devices[i].rt;
        if rt.mode.map(|m| m.value) != Some(mode) {
            rt.mode = Some(Timed { value: mode, at: now });
        }
    }

    fn set_desired_reading(&mut self, i: usize, desired: f32, now: Millis) {
        // FHEM: valves sometimes report a setpoint of 0; ignore it.
        if desired <= 0.0 {
            return;
        }
        self.devices[i].rt.desired = Some(Timed { value: desired, at: now });
    }

    fn apply_status(&mut self, i: usize, status: Status, with_mode: bool, now: Millis) {
        if with_mode {
            self.set_mode(i, status.mode, now);
        }
        let rt = &mut self.devices[i].rt;
        rt.rf_error = Some(status.rf_error);
        rt.low_battery = Some(status.low_battery);
    }

    fn apply_thermostat(&mut self, i: usize, s: &ThermostatState, now: Millis) {
        self.apply_status(i, s.status, true, now);
        self.devices[i].rt.valve = Some(Timed { value: s.valve_position, at: now });
        self.set_desired_reading(i, s.desired, now);
        if let Some(m) = s.measured {
            self.devices[i].rt.measured = Some(Timed { value: m, at: now });
        }
    }

    fn apply_wall_thermostat(&mut self, i: usize, s: &WallThermostatState, now: Millis) {
        self.apply_status(i, s.status, true, now);
        if self.devices[i].cfg.display_actual_temperature != Some(s.display_actual_temperature) {
            self.devices[i].cfg.display_actual_temperature = Some(s.display_actual_temperature);
            self.mark_dirty(now);
        }
        self.set_desired_reading(i, s.desired, now);
        if let Some(m) = s.measured {
            self.devices[i].rt.measured = Some(Timed { value: m, at: now });
        }
    }

    /// An ACK carries the device's state. The old firmware parsed it even from 1-byte ACKs,
    /// reading zeros past the end, which set every acknowledging device to auto mode.
    fn apply_ack_state(&mut self, i: usize, state: &[u8], now: Millis) {
        match self.devices[i].cfg.device_type {
            Some(t) if t.is_heater() => {
                if let Some(s) = parse_thermostat_state(state) {
                    self.apply_thermostat(i, &s, now);
                }
            }
            Some(DeviceType::WallThermostat) => {
                if let Some(s) = parse_wall_thermostat_state(state) {
                    self.apply_wall_thermostat(i, &s, now);
                }
            }
            Some(DeviceType::ShutterContact) => {
                if let Some(&b) = state.first() {
                    self.devices[i].rt.open = Some(b & 0x03 != 0);
                    self.apply_status(i, Status::from_byte(b), false, now);
                }
            }
            _ => {}
        }
    }

    // ----- periodic ------------------------------------------------------------------

    /// Call about once a second.
    pub fn tick(&mut self, ctx: Ctx) -> Vec<Effect> {
        let mut out = Vec::new();
        let now = ctx.now;
        self.now = now;

        if self.booted_at.is_none()
            && let Some(unix) = ctx.unix_time
        {
            let boot = unix.saturating_sub(now / 1000);
            self.booted_at = Some(local_time(boot, &self.settings.timezone).iso());
            self.publish_bridge(&mut out);
        }

        if self.pairing_until.is_some_and(|t| now >= t) {
            log::info!("Pairing off (timeout)");
            self.pairing_until = None;
            self.publish_bridge(&mut out);
        }

        if core::mem::take(&mut self.pending_changed) {
            self.mark_dirty(now);
        }
        // Thermostats always listen: their unconfirmed settings go out again without waiting
        // to hear them (valves in a room without a wall thermostat seldom transmit). One
        // device per tick; window sensors wait until they wake up (see `handle_radio`).
        if let Some(i) = (0..self.devices.len()).find(|&i| {
            let d = &self.devices[i];
            d.is_thermostat()
                && d.cfg.room.is_some()
                && !d.cfg.pending.is_empty()
                && !self.tx.has_for(d.cfg.address)
                && d.rt.resent_at.is_none_or(|t| now.saturating_sub(t) >= RESEND_EVERY)
        }) {
            self.send_pending(i, now);
        }
        if self.dirty_since.is_some_and(|t| now.saturating_sub(t) >= SAVE_DELAY) {
            self.dirty_since = None;
            out.push(Effect::SaveConfig);
        }

        if now.saturating_sub(self.last_time_sync) >= TIME_SYNC_INTERVAL
            && let Some(clock) = self.local(ctx)
        {
            self.last_time_sync = now;
            self.sync_time_slot(&clock);
        }
        if let Some(clock) = self.local(ctx) {
            self.ask_silent_valve(&clock, now);
        }

        if now.saturating_sub(self.last_rx) >= RADIO_WATCHDOG {
            log::warn!("Nothing received for 5 minutes, re-initialising the radio");
            self.last_rx = now;
            out.push(Effect::ReinitRadio);
            let topic = format!("{}/init", self.topics.prefix);
            out.push(Effect::Publish { topic, payload: "RF init".into(), retain: false });
        }

        let demand = self.demand(ctx);
        let was_on = self.burner.on;
        let on = self.burner.update(&self.settings.heating, demand, now);
        let demand_changed =
            libm::fabsf(demand.output - self.demand.output) >= 1.0 || demand.boost != self.demand.boost;
        self.demand = demand;
        if on != was_on {
            log::info!(
                "Burner {} (demand {:.1} %{})",
                if on { "ON" } else { "OFF" },
                demand.output,
                if demand.boost { ", boost" } else { "" }
            );
        }
        if on != was_on || demand_changed {
            self.publish_bridge(&mut out);
        }
        // "heating" follows the burner, the radiators and their readings going stale.
        for i in 0..self.devices.len() {
            let heating = self.heating(i);
            if heating.is_some() && heating != self.devices[i].rt.heating_sent {
                self.devices[i].rt.heating_sent = heating;
                self.publish_device(i, &mut out);
            }
        }
        out
    }

    /// A radiator valve does not always report by itself (one 21 % open was silent for an
    /// hour), and a reading older than `stale_minutes` stops counting for the burner. Before
    /// that happens, send the valve the time: harmless, and its ACK carries its state. A valve
    /// that reports by itself is never asked. One that does not answer is asked again after
    /// `ASK_AGAIN`, not on every tick. One valve per tick.
    fn ask_silent_valve(&mut self, clock: &LocalTime, now: Millis) {
        let stale = self.settings.heating.stale_minutes as Millis * MINUTE;
        let ask_after = stale * 4 / 5;
        let found = self.devices.iter().position(|d| {
            // Not heard since start counts from start, so a restart gets the valves' state.
            let heard = d.rt.valve.map_or(0, |v| v.at);
            d.is_heater()
                && d.cfg.room.is_some()
                && now.saturating_sub(heard) >= ask_after
                && d.rt.asked_at.is_none_or(|t| t < heard || now.saturating_sub(t) >= ASK_AGAIN)
                && !self.tx.has_for(d.cfg.address)
        });
        if let Some(i) = found {
            let d = &mut self.devices[i];
            let silent = now.saturating_sub(d.rt.valve.map_or(0, |v| v.at)) / 1000;
            log::info!("Sending {} the time to get its state, silent for {silent} s", d.cfg.name);
            d.rt.asked_at = Some(now);
            let addr = d.cfg.address;
            self.send_time(addr, clock);
        }
    }

    fn sync_time_slot(&mut self, clock: &LocalTime) {
        let slot = self.time_sync_slot;
        self.time_sync_slot = (slot + 1) % TIME_SYNC_SLOTS;
        let targets: Vec<Address> = self
            .devices
            .iter()
            .enumerate()
            .filter(|(idx, d)| idx % TIME_SYNC_SLOTS == slot && d.is_thermostat() && d.cfg.room.is_some())
            .map(|(_, d)| d.cfg.address)
            .collect();
        for addr in targets {
            self.send_time(addr, clock);
        }
    }

    /// The heat demand: the most-open fresh valve among devices with a room, in any mode.
    ///
    /// Only devices with a room count. With autocreate on, the old firmware let any
    /// neighbour's radiator valve in manual mode switch our burner on. A valve during its
    /// weekly descaling (decalc) hour is ignored: it opens fully then, whatever the room needs.
    pub fn demand(&self, ctx: Ctx) -> Demand {
        let now = ctx.now;
        let h = &self.settings.heating;
        let stale = h.stale_minutes as Millis * MINUTE;
        let fresh = |at: Millis| now.saturating_sub(at) <= stale;
        let clock = self.local(ctx);

        let mut demand = Demand::default();
        for d in self.devices.iter().filter(|d| d.cfg.room.is_some() && d.is_thermostat()) {
            if let Some(mode) = d.rt.mode
                && mode.value == Mode::Boost
                && d.rt.last_seen.is_some_and(fresh)
                && boost_active(d.cfg.valve.boost_minutes, mode.at, now)
            {
                demand.boost = true;
            }
            if !d.is_heater() {
                continue;
            }
            let decalc = clock
                .as_ref()
                .is_some_and(|c| c.weekday == d.cfg.valve.decalc_day && c.hour == d.cfg.valve.decalc_hour);
            // A valve gone silent is asked for its state before its reading expires
            // (`ask_silent_valve`). A reading older than `stale_minutes` is ignored, which
            // counts as 0 %, the same as a closed valve: a valve that does not answer either
            // (flat battery, out of range) cannot keep the burner going.
            if let Some(v) = d.rt.valve.filter(|v| fresh(v.at))
                && !decalc
            {
                demand.output = demand.output.max(valve_demand(h, v.value));
            }
        }
        demand
    }

    // ----- publishing ----------------------------------------------------------------

    fn publish_bridge(&self, out: &mut Vec<Effect>) {
        let topic = self.topics.prefix.clone();
        let payload = serde_json::to_string(&self.bridge_state()).unwrap_or_default();
        out.push(Effect::Publish { topic, payload, retain: true });
    }

    fn bridge_state(&self) -> BridgeState<'_> {
        BridgeState {
            availability: "online",
            address: self.settings.address,
            pairing_enabled: self.pairing_until.is_some(),
            autocreate: self.settings.autocreate,
            furnace_running: self.burner.on,
            heat_demand: round1(self.demand.output),
            tx_queue: self.tx.active(),
            booted_at: self.booted_at.as_deref(),
        }
    }

    fn publish_device(&self, i: usize, out: &mut Vec<Effect>) {
        let d = &self.devices[i];
        if d.cfg.name.is_empty() {
            return;
        }
        out.push(Effect::Publish {
            topic: self.state_topic(&d.cfg.name),
            payload: serde_json::to_string(&self.device_state(i)).unwrap_or_default(),
            retain: true,
        });
    }

    /// The most-open radiator in device `i`'s room, by the same freshness rule as the
    /// burner's demand: a valve gone silent does not count.
    fn room_valve(&self, i: usize) -> Option<u8> {
        let d = &self.devices[i];
        let stale = self.settings.heating.stale_minutes as Millis * MINUTE;
        self.devices
            .iter()
            .filter(|o| o.is_heater() && o.cfg.room.is_some() && o.cfg.room == d.cfg.room)
            .filter(|o| d.is_heater().then_some(o.cfg.address).is_none_or(|a| a == d.cfg.address))
            .filter_map(|o| o.rt.valve.filter(|v| self.now.saturating_sub(v.at) <= stale).map(|v| v.value))
            .max()
    }

    /// A device of ours sending to a device that is not its partner still holds a link the
    /// bridge does not know about: left from another controller, or from a room change whose
    /// unlink got lost. It keeps trying to reach that partner and reports a radio error
    /// (a radiator called a hallway radiator every 10 s). Tell it to drop the link, once
    /// per partner since start; the unlink is tracked until confirmed, a rejection included.
    fn drop_stale_link(&mut self, i: usize, f: &Frame, msg: &Message) {
        let d = &self.devices[i];
        let partner_talk = !matches!(
            msg,
            Message::Ack { .. } | Message::TimeRequest | Message::TimeInformation | Message::PairPing { .. }
        );
        if !partner_talk
            || f.dst == self.settings.address
            || f.dst.is_broadcast()
            || d.cfg.room.is_none()
            || d.cfg.associations.contains(&f.dst)
            || d.rt.dropped_links.contains(&f.dst)
        {
            return;
        }
        // The unlink names the partner's type, so only partners the bridge knows.
        let Some(t) = self.find(f.dst).and_then(|j| self.devices[j].cfg.device_type) else { return };
        log::warn!("{} still talks to {}, which is not its partner: unlinking", d.cfg.name, f.dst);
        self.devices[i].rt.dropped_links.insert(f.dst);
        self.send(f.src, command::REMOVE_LINK_PARTNER, payload::link_partner(f.dst, t));
    }

    /// A device puts the group it holds in the header of what it sends. One in a room using
    /// another group than its room's (left from a room change, or from another controller)
    /// takes commands meant for that other group: a bathroom boost boosted a bedroom. Send
    /// it its group again, once since start; it is tracked until confirmed.
    fn fix_group(&mut self, i: usize, f: &Frame) {
        let d = &self.devices[i];
        let want = d.cfg.group;
        if f.group == 0 || want == 0 || f.group == want || d.cfg.room.is_none() || d.rt.group_fixed {
            return;
        }
        log::warn!("{} uses group {}, its room's is {want}: sending it again", d.cfg.name, f.group);
        self.devices[i].rt.group_fixed = true;
        self.send(f.src, command::SET_GROUP_ID, payload::set_group(want));
    }

    /// A device's mode. A wall thermostat's broadcasts carry no mode, so after a restart it
    /// is unknown until the thermostat sends its full state; its radiators follow it and say
    /// it in theirs, so take theirs until then.
    fn mode(&self, i: usize) -> Option<Mode> {
        let d = &self.devices[i];
        d.rt.mode.map(|m| m.value).or_else(|| {
            (d.cfg.device_type == Some(DeviceType::WallThermostat) && d.cfg.room.is_some())
                .then(|| {
                    self.devices
                        .iter()
                        .filter(|o| o.is_heater() && o.cfg.room == d.cfg.room)
                        .find_map(|o| o.rt.mode.map(|m| m.value))
                })
                .flatten()
        })
    }

    /// What Home Assistant shows as "heating": the burner is on, and this thermostat's room
    /// (for a radiator: the radiator itself) calls for heat. An open valve alone is not
    /// heating between burner cycles, nor is one whose reading went stale.
    fn heating(&self, i: usize) -> Option<bool> {
        let d = &self.devices[i];
        (d.is_thermostat() && d.cfg.room.is_some()).then(|| {
            self.burner.on && self.room_valve(i).is_some_and(|v| v >= self.settings.heating.min_valve_position)
        })
    }

    fn device_state(&self, i: usize) -> DeviceState<'_> {
        let d = &self.devices[i];
        let valve = if d.cfg.device_type == Some(DeviceType::WallThermostat) {
            // Highest valve in the room, as the old firmware published.
            self.room_valve(i)
        } else {
            d.rt.valve.map(|v| v.value)
        };
        let heating = self.heating(i);
        DeviceState {
            address: d.cfg.address,
            device_type: d.cfg.device_type.map(DeviceType::as_str),
            room: d.cfg.room.as_deref(),
            mode: self.mode(i).map(Mode::as_str),
            measured_temperature: d.rt.measured.map(|t| round1(t.value)),
            desired_temperature: d.rt.desired.map(|t| round1(t.value)),
            valve_position: valve,
            heating,
            open: d.rt.open,
            low_battery: d.rt.low_battery,
            rf_error: d.rt.rf_error,
            rssi: d.rt.rssi,
        }
    }

    // ----- Home Assistant ------------------------------------------------------------

    fn discovery(&self) -> Option<Discovery<'_>> {
        let discovery_prefix = self.topics.discovery.as_deref()?;
        Some(Discovery { discovery_prefix, prefix: &self.topics.prefix, bridge: self.settings.address })
    }

    /// Announce the bridge and every device with a room, from scratch.
    /// The devices follow through [`Self::next_announcement`].
    fn announce_all(&mut self, out: &mut Vec<Effect>) {
        let Some(bridge) = self.discovery().map(|d| d.bridge()) else { return };
        out.push(Effect::Publish { topic: bridge.0, payload: bridge.1, retain: true });
        for d in &mut self.devices {
            d.rt.announced = None;
        }
        // Keep publishing states if a reconnect replay is still running.
        let states = self.replay.is_some_and(|(_, s)| s);
        self.replay = Some((0, states));
    }

    /// Tell Home Assistant about a device if its name, room or type changed; remove it when
    /// it no longer has a room.
    fn announce(&mut self, i: usize, out: &mut Vec<Effect>) {
        let Some(disco) = self.discovery() else { return };
        let d = &self.devices[i];
        let want = match (&d.cfg.room, d.cfg.device_type) {
            (Some(room), Some(t)) if t != DeviceType::Cube && !d.cfg.name.is_empty() => {
                Some((d.cfg.name.clone(), room.clone(), t))
            }
            _ => None,
        };
        if want == d.rt.announced {
            return;
        }
        let (topic, payload) = match &want {
            Some((_, _, t)) => disco.device(&d.cfg, *t, d.rt.announced.as_ref().map(|a| a.2)),
            None => (disco.device_topic(d.cfg.address), String::new()),
        };
        out.push(Effect::Publish { topic, payload, retain: true });
        self.devices[i].rt.announced = want;
    }

    /// After a settings change: state (if we have any) and discovery.
    fn device_changed(&mut self, i: usize, out: &mut Vec<Effect>) {
        if self.devices[i].rt.last_seen.is_some() {
            self.publish_device(i, out);
        }
        self.announce(i, out);
    }

    // ----- web API -------------------------------------------------------------------

    /// Bridge settings, as on the `max/set` topic.
    pub fn configure_bridge(&mut self, v: &Value, ctx: Ctx) -> Vec<Effect> {
        let mut out = Vec::new();
        self.set_bridge(v, ctx, &mut out);
        out
    }

    /// Device settings and commands, as on `max/<name>/set`, plus `"name"` to rename.
    /// A device not heard yet is added when `v` has a `"name"`, so devices that seldom
    /// transmit can be set up in advance.
    pub fn configure_device(&mut self, address: Address, v: &Value, ctx: Ctx) -> Result<Vec<Effect>, String> {
        let i = match self.find(address) {
            Some(i) => i,
            None if v.get("name").is_some() && !address.is_broadcast() && address != self.settings.address => {
                log::info!("Adding {address} before it was heard");
                self.devices.push(Device {
                    cfg: DeviceConfig { address, name: address.to_string(), ..Default::default() },
                    rt: Runtime::default(),
                });
                self.mark_dirty(ctx.now);
                self.devices.len() - 1
            }
            None => return Err(format!("no device {address}")),
        };
        if !v.is_object() {
            return Err("expected a JSON object".into());
        }
        if let Some(week) = v.get("room_schedule") {
            validate_week(week)?;
        }
        let mut out = Vec::new();
        if let Some(name) = v.get("name") {
            let name = name.as_str().ok_or("name must be a string")?;
            self.rename_device(i, name, ctx, &mut out)?;
        }
        self.set_device(i, v, ctx);
        self.device_changed(i, &mut out);
        Ok(out)
    }

    /// Remove a device: unlink it from the devices it is linked with, drop what is queued
    /// for it, clear its MQTT topics and remove it from Home Assistant. If it is heard
    /// again (and autocreate is on), it comes back as a new, unconfigured device.
    pub fn forget_device(&mut self, address: Address, ctx: Ctx) -> Result<Vec<Effect>, String> {
        let i = self.find(address).ok_or_else(|| format!("no device {address}"))?;
        let mut out = Vec::new();
        let name = self.devices[i].cfg.name.clone();
        log::info!("Forgetting {name} ({address})");
        if let Some(t) = self.devices[i].cfg.device_type {
            for j in 0..self.devices.len() {
                if j != i && self.devices[j].cfg.associations.contains(&address) {
                    let partner = self.devices[j].cfg.address;
                    self.send(partner, command::REMOVE_LINK_PARTNER, payload::link_partner(address, t));
                    self.devices[j].cfg.associations.retain(|&a| a != address);
                }
            }
        }
        if let Some(disco) = self.discovery() {
            out.push(Effect::Publish { topic: disco.device_topic(address), payload: String::new(), retain: true });
        }
        if !name.is_empty() {
            out.push(Effect::Unsubscribe(self.set_topic(&name)));
            out.push(Effect::Publish { topic: self.state_topic(&name), payload: String::new(), retain: true });
        }
        self.tx.forget(address);
        self.devices.remove(i);
        self.mark_dirty(ctx.now);
        Ok(out)
    }

    /// Everything the web UI shows: bridge state, and each device's state with how long
    /// ago it was heard.
    pub fn status(&self, ctx: Ctx) -> WebStatus<'_> {
        let bridge = BridgeStatus {
            state: self.bridge_state(),
            duty_cycle_limit: self.settings.duty_cycle_limit,
            timezone: self.settings.timezone,
            heating: &self.settings.heating,
            credit_ms: self.tx.credit_ms(),
            tx_waiting: self.tx.parked(),
            pairing_seconds_left: self.pairing_until.map(|until| until.saturating_sub(ctx.now) / 1000),
            time: self.local(ctx).map(|t| t.iso()),
        };
        let devices = (0..self.devices.len())
            .map(|i| DeviceStatus {
                state: self.device_state(i),
                name: &self.devices[i].cfg.name,
                unconfirmed: self.devices[i].cfg.pending.len(),
                seen_seconds_ago: self.devices[i].rt.last_seen.map(|seen| ctx.now.saturating_sub(seen) / 1000),
            })
            .collect();
        WebStatus { bridge, devices }
    }
}

// Published and served state, serialised straight to text (no JSON tree in between).

/// `max` (retained).
#[derive(Serialize)]
pub struct BridgeState<'a> {
    availability: &'static str,
    address: Address,
    pairing_enabled: bool,
    autocreate: bool,
    furnace_running: bool,
    heat_demand: f64,
    tx_queue: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    booted_at: Option<&'a str>,
}

/// `max/<name>` (retained).
#[derive(Serialize)]
pub struct DeviceState<'a> {
    address: Address,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    device_type: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    room: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    measured_temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    desired_temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    valve_position: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    heating: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    low_battery: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rf_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rssi: Option<i16>,
}

/// What the web UI shows, see [`Bridge::status`].
#[derive(Serialize)]
pub struct WebStatus<'a> {
    pub bridge: BridgeStatus<'a>,
    pub devices: Vec<DeviceStatus<'a>>,
}

#[derive(Serialize)]
pub struct BridgeStatus<'a> {
    #[serde(flatten)]
    state: BridgeState<'a>,
    duty_cycle_limit: bool,
    timezone: TimeZone,
    heating: &'a crate::config::HeatingConfig,
    credit_ms: u32,
    /// Commands parked until their sleeping device wakes up.
    tx_waiting: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pairing_seconds_left: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time: Option<String>,
}

#[derive(Serialize)]
pub struct DeviceStatus<'a> {
    #[serde(flatten)]
    state: DeviceState<'a>,
    name: &'a str,
    /// Settings sent but not acknowledged yet.
    unconfirmed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    seen_seconds_ago: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::days_from_civil;
    use serde_json::json;

    const ME: Address = Address([0x12, 0x34, 0x56]);
    const WT: Address = Address([0x0A, 0x00, 0x01]);
    const HT: Address = Address([0x0B, 0x00, 0x02]);
    const NEIGHBOUR: Address = Address([0x0C, 0x00, 0x03]);

    fn ctx(now: Millis) -> Ctx {
        Ctx { now, unix_time: Some((days_from_civil(2026, 11, 16) * 86_400) as u64 + 8 * 3600 + now / 1000) }
    }

    fn rx(src: Address, dst: Address, msgcnt: u8, command: u8, payload: Vec<u8>) -> Vec<u8> {
        let mut raw = Frame { msgcnt, flags: 0, command, src, dst, group: 0, payload }.encode();
        raw.extend_from_slice(&[0x10, 0x80]);
        raw
    }

    fn bridge() -> Bridge {
        Bridge::new(Config { duty_cycle_limit: false, ..Config::default() }, Topics::default(), 0)
    }

    /// Send everything queued, acknowledging each command, and return the frames.
    fn drain(b: &mut Bridge, now: Millis) -> Vec<Frame> {
        let mut sent = Vec::new();
        for _ in 0..100 {
            match b.next_tx(ctx(now)) {
                Next::Send { id, bytes, .. } => {
                    b.transmitted(id, now);
                    let mut raw = bytes.clone();
                    raw.extend_from_slice(&[0, 0x80]);
                    let f = decode_rx(&raw).unwrap().frame;
                    if let TxId::Command(_) = id {
                        // As if the device answered over the radio.
                        let mut events = Vec::new();
                        b.tx.ack(f.dst, f.msgcnt, true, &mut events);
                        b.log_events(events);
                    }
                    sent.push(f);
                }
                _ => break,
            }
        }
        sent
    }

    fn publishes(effects: &[Effect], topic: &str) -> Vec<Value> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Publish { topic: t, payload, .. } if t == topic => serde_json::from_str(payload).ok(),
                _ => None,
            })
            .collect()
    }

    /// Connect plus every announcement after it, as the firmware sends them.
    fn connect(b: &mut Bridge) -> Vec<Effect> {
        let mut out = b.on_mqtt_connected();
        while let Some(more) = b.next_announcement() {
            out.extend(more);
        }
        out
    }

    fn mqtt(b: &mut Bridge, topic: &str, v: Value, now: Millis) -> Vec<Effect> {
        b.handle_mqtt(topic, v.to_string().as_bytes(), false, ctx(now))
    }

    /// Configuration as the web UI sends it, addressed like the old MQTT topics:
    /// `max/set`, `max/rename` and `max/<name>/set`.
    fn web(b: &mut Bridge, topic: &str, v: Value, now: Millis) -> Vec<Effect> {
        let rest = topic.strip_prefix("max/").unwrap();
        if rest == "set" {
            return b.configure_bridge(&v, ctx(now));
        }
        if rest == "rename" {
            let a: Address = v["address"].as_str().unwrap().parse().unwrap();
            return b.configure_device(a, &json!({ "name": v["to"] }), ctx(now)).unwrap_or_default();
        }
        let name = rest.strip_suffix("/set").unwrap();
        let a = b.devices[b.find_by_name(name).expect(name)].cfg.address;
        b.configure_device(a, &v, ctx(now)).unwrap_or_default()
    }

    /// A living room: wall thermostat WT at 19.0 wanting 21.0, radiator valve HT at 50 %.
    fn living_room(b: &mut Bridge, now: Millis) {
        b.handle_radio(&rx(WT, ME, 1, command::WALL_THERMOSTAT_STATE, vec![0x01, 0x00, 42, 0, 0, 0, 190]), ctx(now));
        b.handle_radio(&rx(HT, ME, 1, command::THERMOSTAT_STATE, vec![0x01, 50, 42, 0, 200]), ctx(now));
        web(b, "max/rename", json!({"address": "0A0001", "to": "living-room/thermostat"}), now);
        web(b, "max/rename", json!({"address": "0B0002", "to": "living-room/heater"}), now);
        web(b, "max/living-room/thermostat/set", json!({"room": "living-room"}), now);
        web(b, "max/living-room/heater/set", json!({"room": "living-room"}), now);
    }

    #[test]
    fn autocreates_and_publishes_state_and_subscribes() {
        let mut b = bridge();
        let out = b.handle_radio(&rx(HT, ME, 3, command::THERMOSTAT_STATE, vec![0x01, 40, 43, 0, 212]), ctx(0));
        assert!(out.contains(&Effect::Subscribe("max/0B0002/set".into())), "{out:?}");
        let state = &publishes(&out, "max/0B0002")[0];
        assert_eq!(state["type"], "heater");
        assert_eq!(state["mode"], "manual");
        assert_eq!(state["valve_position"], 40);
        assert_eq!(state["desired_temperature"], 21.5);
        assert_eq!(state["measured_temperature"], 21.2);
    }

    #[test]
    fn uppercase_default_names_can_be_controlled() {
        // The old firmware lowercased the topic, so "max/0B0002/set" never matched.
        let mut b = bridge();
        b.handle_radio(&rx(HT, ME, 3, command::THERMOSTAT_STATE, vec![0x01, 40, 43, 0, 212]), ctx(0));
        mqtt(&mut b, "max/0B0002/set", json!({"temperature": 22}), 0);
        let sent = drain(&mut b, 0);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].command, command::SET_TEMPERATURE);
    }

    #[test]
    fn ack_without_pending_command_is_harmless() {
        let mut b = bridge();
        b.handle_radio(&rx(HT, ME, 9, command::ACK, vec![0x01]), ctx(0));
    }

    #[test]
    fn one_byte_ack_does_not_change_mode() {
        let mut b = bridge();
        b.handle_radio(&rx(HT, ME, 1, command::THERMOSTAT_STATE, vec![0x01, 40, 43, 0, 212]), ctx(0));
        b.handle_radio(&rx(HT, ME, 2, command::ACK, vec![0x01]), ctx(1));
        assert_eq!(b.devices[0].rt.mode.unwrap().value, Mode::Manual);
    }

    #[test]
    fn ack_state_updates_valve() {
        let mut b = bridge();
        b.handle_radio(&rx(HT, ME, 1, command::THERMOSTAT_STATE, vec![0x01, 40, 43, 0, 212]), ctx(0));
        b.handle_radio(&rx(HT, ME, 2, command::ACK, vec![0x01, 0x00, 75, 44]), ctx(1));
        let rt = &b.devices[0].rt;
        assert_eq!(rt.valve.unwrap().value, 75);
        assert_eq!(rt.mode.unwrap().value, Mode::Auto);
        assert_eq!(rt.desired.unwrap().value, 22.0);
    }

    #[test]
    fn temperature_change_keeps_auto_mode() {
        let mut b = bridge();
        b.handle_radio(&rx(HT, ME, 1, command::THERMOSTAT_STATE, vec![0x00, 40, 43, 0, 212]), ctx(0));
        mqtt(&mut b, "max/0B0002/set", json!({"temperature": 22}), 0);
        let sent = drain(&mut b, 0);
        assert_eq!(sent[0].payload, vec![44], "auto mode, 22 °C");

        b.handle_radio(&rx(HT, ME, 2, command::THERMOSTAT_STATE, vec![0x01, 40, 43, 0, 212]), ctx(1));
        mqtt(&mut b, "max/0B0002/set", json!({"temperature": "eco"}), 1);
        assert_eq!(drain(&mut b, 1)[0].payload, vec![0x40 | 34], "manual, eco 17 °C");
    }

    #[test]
    fn mode_without_temperature() {
        let mut b = bridge();
        b.handle_radio(&rx(HT, ME, 1, command::ACK, vec![0x01]), ctx(0));
        mqtt(&mut b, "max/0B0002/set", json!({"mode": "manual"}), 0);
        assert!(drain(&mut b, 0).is_empty(), "no setpoint known: nothing sent (was -1 °C)");
        mqtt(&mut b, "max/0B0002/set", json!({"mode": "auto"}), 0);
        assert_eq!(drain(&mut b, 0)[0].payload, vec![0x00]);
        mqtt(&mut b, "max/0B0002/set", json!({"mode": "temporary", "temperature": 20}), 0);
        assert!(drain(&mut b, 0).is_empty(), "temporary without until");
        mqtt(&mut b, "max/0B0002/set", json!({"mode": "temporary", "temperature": 20, "until": "2026-11-20 18:30"}), 0);
        let p = &drain(&mut b, 0)[0].payload;
        assert_eq!(p.len(), 4);
        assert_eq!(p[0], 0x80 | 40);
        mqtt(&mut b, "max/0B0002/set", json!({"mode": "bogus"}), 0);
        assert!(drain(&mut b, 0).is_empty(), "unknown mode is not manual");
    }

    #[test]
    fn group_goes_into_set_temperature_header() {
        let mut b = bridge();
        b.handle_radio(&rx(HT, ME, 1, command::ACK, vec![0x01]), ctx(0));
        web(&mut b, "max/0B0002/set", json!({"group": 2, "temperature": 21}), 0);
        let sent = drain(&mut b, 0);
        assert_eq!(sent[0].command, command::SET_GROUP_ID);
        assert_eq!(sent[0].payload, vec![2]);
        assert_eq!((sent[1].flags, sent[1].group), (FLAG_GROUP, 2));
    }

    #[test]
    fn valve_settings() {
        let mut b = bridge();
        b.handle_radio(&rx(HT, ME, 1, command::ACK, vec![0x01]), ctx(0));
        web(&mut b, "max/0B0002/set", json!({"decalc_weekday": "monday", "decalc_hour": 4, "boost_duration": 60}), 0);
        let sent = drain(&mut b, 0);
        // The old firmware turned "monday" into 1 (boolean OR) = Sunday.
        assert_eq!(sent[0].payload[1], (2 << 5) | 4);
        assert_eq!(sent[0].payload[0] >> 5, 7);
        assert_eq!(b.devices[0].cfg.valve.boost_minutes, 60);
        web(&mut b, "max/0B0002/set", json!({"decalc_weekday": "someday"}), 0);
        assert_eq!(b.devices[0].cfg.valve.decalc_day, Weekday::Monday);
    }

    #[test]
    fn schedule_from_readme() {
        let mut b = bridge();
        b.handle_radio(&rx(WT, ME, 1, command::ACK, vec![0x01]), ctx(0));
        web(&mut b, "max/0A0001/set", json!({"day": "monday", "schedule": {"6:00": 21.5, "22:30": 4.5}}), 0);
        let sent = drain(&mut b, 0);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].command, command::CONFIG_WEEK_PROFILE);
        assert_eq!(sent[0].payload[0], 2);
        let stored = &b.devices[0].cfg.schedule[&Weekday::Monday];
        assert_eq!(stored.entries().collect::<Vec<_>>(), vec![(6 * 60, 21.5), (22 * 60 + 30, 4.5)]);

        web(&mut b, "max/0A0001/set", json!({"day": "funday", "schedule": {"6:00": 21.5}}), 0);
        assert!(drain(&mut b, 0).is_empty());
        web(&mut b, "max/0A0001/set", json!({"schedule": {"tuesday": {"7:00": 20}, "wednesday": {"8:00": 19}}}), 0);
        assert_eq!(drain(&mut b, 0).len(), 2);
    }

    #[test]
    fn rename_rules() {
        let mut b = bridge();
        b.handle_radio(&rx(WT, ME, 1, command::ACK, vec![0x01]), ctx(0));
        b.handle_radio(&rx(HT, ME, 1, command::ACK, vec![0x01]), ctx(0));
        let out = web(&mut b, "max/rename", json!({"address": "0A0001", "to": "living-room/thermostat"}), 0);
        assert!(out.contains(&Effect::Unsubscribe("max/0A0001/set".into())));
        assert!(out.contains(&Effect::Publish { topic: "max/0A0001".into(), payload: String::new(), retain: true }));
        assert!(out.contains(&Effect::Subscribe("max/living-room/thermostat/set".into())));
        for bad in ["living-room/thermostat", "set", "x/set", "a+b", "", "/x"] {
            let out = web(&mut b, "max/rename", json!({"address": "0B0002", "to": bad}), 0);
            assert!(out.is_empty(), "{bad:?}");
        }
        assert_eq!(b.devices[1].cfg.name, "0B0002");
    }

    #[test]
    fn answers_devices_immediately_with_short_preamble() {
        let mut b = bridge();
        b.handle_radio(&rx(HT, ME, 1, command::ACK, vec![0x01]), ctx(0));
        mqtt(&mut b, "max/0B0002/set", json!({"temperature": 22}), 0);
        let Next::Send { id, .. } = b.next_tx(ctx(0)) else { panic!() };
        b.transmitted(id, 1000);
        // While that waits for its ACK, the wall thermostat sends us a SetTemperature.
        b.handle_radio(&rx(WT, ME, 0x42, command::SET_TEMPERATURE, vec![0x40 | 42]), ctx(1100));
        match b.next_tx(ctx(1100)) {
            Next::Send { long_preamble: false, bytes, .. } => {
                assert_eq!(bytes[1], 0x42);
                assert_eq!(bytes[3], command::ACK);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn pair_ping_handling() {
        let mut b = bridge();
        let ping = |dst| rx(WT, dst, 0, command::PAIR_PING, vec![0x10, 0x03, 0, b'K', b'E', b'Q']);
        // Paired to another cube: ignored even with pairing on.
        mqtt(&mut b, "max/set", json!({"pair": true}), 0);
        b.handle_radio(&ping(NEIGHBOUR), ctx(0));
        assert!(drain(&mut b, 0).is_empty());
        // Broadcast while pairing: pong first, then the configuration.
        b.handle_radio(&ping(Address::BROADCAST), ctx(1));
        let sent = drain(&mut b, 1);
        assert_eq!(sent[0].command, command::PAIR_PONG);
        assert!(sent.iter().any(|f| f.command == command::CONFIG_TEMPERATURES));
        assert!(sent.iter().any(|f| f.command == command::TIME_INFORMATION));
        // Re-pairing to us: pong only.
        mqtt(&mut b, "max/set", json!({"pair": false}), 2);
        b.handle_radio(&ping(ME), ctx(2));
        let sent = drain(&mut b, 2);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].command, command::PAIR_PONG);
    }

    #[test]
    fn pairing_switches_itself_off() {
        let mut b = bridge();
        let ping = |cnt| rx(WT, Address::BROADCAST, cnt, command::PAIR_PING, vec![0x10, 0x03, 0, b'K', b'E', b'Q']);
        mqtt(&mut b, "max/set", json!({"pair": true}), 0);
        assert_eq!(serde_json::to_value(b.status(ctx(60_000))).unwrap()["bridge"]["pairing_seconds_left"], 240);
        let out = b.tick(ctx(PAIRING_WINDOW));
        // The last bridge state (the first tick also publishes the boot time).
        assert_eq!(publishes(&out, "max").last().unwrap()["pairing_enabled"], false);
        b.handle_radio(&ping(1), ctx(PAIRING_WINDOW + 1000));
        assert!(drain(&mut b, PAIRING_WINDOW + 1000).is_empty(), "no pong after the window");
    }

    #[test]
    fn time_request_is_answered_with_local_time() {
        let mut b = bridge();
        b.handle_radio(&rx(WT, ME, 0x33, command::TIME_INFORMATION, vec![]), ctx(5_000));
        let sent = drain(&mut b, 5_000);
        assert_eq!(sent[0].msgcnt, 0x33);
        // 2026-11-16 08:00:05 UTC is 09:00:05 CET.
        assert_eq!(&sent[0].payload[..3], &[26, 16, 9]);
    }

    #[test]
    fn open_valve_heats_in_any_mode() {
        let mut b = bridge();
        living_room(&mut b, 0);
        // Auto mode, valve 50 %: the old firmware ignored valves in auto.
        b.handle_radio(&rx(HT, ME, 2, command::THERMOSTAT_STATE, vec![0x00, 50, 42, 0, 200]), ctx(1_000));
        assert_eq!(b.demand(ctx(1_000)).output, 50.0);
        b.tick(ctx(1_000));
        assert!(b.burner_on());
    }

    #[test]
    fn valve_that_is_only_just_open_does_not_heat() {
        let mut b = bridge();
        living_room(&mut b, 0);
        b.handle_radio(&rx(HT, ME, 2, command::THERMOSTAT_STATE, vec![0x01, 10, 42, 0, 200]), ctx(1_000));
        assert_eq!(b.demand(ctx(1_000)).output, 0.0);
    }

    #[test]
    fn valve_position_from_ack_to_wall_thermostat_counts() {
        let mut b = bridge();
        living_room(&mut b, 0);
        // The valve answers the wall thermostat with its state: manual, 80 %, 21 °C.
        b.handle_radio(&rx(HT, WT, 3, command::ACK, vec![0x01, 0x01, 80, 42]), ctx(1_000));
        assert_eq!(b.demand(ctx(1_000)).output, 80.0);
    }

    #[test]
    fn neighbours_valve_does_not_heat() {
        let mut b = bridge();
        // Valve 100 %, but no room assigned.
        b.handle_radio(
            &rx(NEIGHBOUR, Address([9, 9, 9]), 1, command::THERMOSTAT_STATE, vec![0x01, 100, 50, 0, 150]),
            ctx(0),
        );
        assert_eq!(b.demand(ctx(0)), Demand::default());
        b.tick(ctx(0));
        assert!(!b.burner_on());
    }

    #[test]
    fn readings_go_stale() {
        let mut b = bridge();
        living_room(&mut b, 0);
        assert!(b.demand(ctx(0)).output > 0.0);
        assert_eq!(b.demand(ctx(11 * MINUTE)).output, 0.0);
    }

    #[test]
    fn heating_means_the_burner_runs_for_the_room() {
        let mut b = bridge();
        living_room(&mut b, 0);
        let heating = |b: &Bridge, name: &str| b.device_state(b.find_by_name(name).unwrap()).heating;
        // Valve at 50 %, burner not decided yet: open, but not heating.
        assert_eq!(heating(&b, "living-room/heater"), Some(false));
        let out = b.tick(ctx(1_000));
        assert!(b.burner_on());
        assert_eq!(heating(&b, "living-room/heater"), Some(true));
        assert_eq!(heating(&b, "living-room/thermostat"), Some(true));
        // Home Assistant hears about it without waiting for the devices.
        assert_eq!(publishes(&out, "max/living-room/heater")[0]["heating"], true);
    }

    #[test]
    fn heating_ignores_stale_rooms_and_follows_radiators() {
        let mut b = bridge();
        living_room(&mut b, 0);
        b.tick(ctx(1_000));
        let wt = b.find(WT).unwrap();
        assert_eq!(b.heating(wt), Some(true));
        // The valve closes while the burner holds on (minimum on time): the wall thermostat
        // is published as no longer heating, without having transmitted.
        b.handle_radio(&rx(HT, ME, 2, command::THERMOSTAT_STATE, vec![0x01, 0, 42, 0, 200]), ctx(2_000));
        let out = b.tick(ctx(3_000));
        assert!(b.burner_on(), "held on by the minimum on time");
        assert_eq!(publishes(&out, "max/living-room/thermostat")[0]["heating"], false);
        // Open again, then silent past stale_minutes: not heating, even with the burner on.
        b.handle_radio(&rx(HT, ME, 3, command::THERMOSTAT_STATE, vec![0x01, 80, 42, 0, 200]), ctx(4_000));
        b.tick(ctx(5_000));
        assert_eq!(b.heating(wt), Some(true));
        b.burner.on = true;
        b.now = 4_000 + 11 * MINUTE;
        assert_eq!(b.heating(wt), Some(false));
    }

    #[test]
    fn unlink_stored_by_the_previous_firmware_completes() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        let wt = b.find(WT).unwrap();
        b.devices[wt].cfg.pending.insert(Pending::Unlink(HT));
        b.send_pending(wt, 1);
        drain(&mut b, 1);
        assert!(b.devices[wt].cfg.pending.is_empty(), "{:?}", b.devices[wt].cfg.pending);
    }

    #[test]
    fn wall_thermostat_takes_its_radiators_mode_until_it_reports_one() {
        let mut b = Bridge::new(Config { duty_cycle_limit: false, ..Config::default() }, Topics::default(), 0);
        living_room(&mut b, 0);
        let mut c = b.config();
        c.tracking = true;
        let mut b = Bridge::new(c, Topics::default(), 0);
        let wt = b.find(WT).unwrap();
        assert_eq!(b.mode(wt), None);
        // A broadcast to its radiator: temperatures only.
        b.handle_radio(&rx(WT, HT, 5, command::WALL_THERMOSTAT_CONTROL, vec![44, 210]), ctx(1_000));
        assert_eq!(b.mode(wt), None);
        b.handle_radio(&rx(HT, ME, 6, command::THERMOSTAT_STATE, vec![0x01, 50, 44, 0, 200]), ctx(2_000));
        assert_eq!(b.mode(wt), Some(Mode::Manual));
    }

    #[test]
    fn config_in_pieces_is_the_whole_config() {
        let mut b = bridge();
        living_room(&mut b, 0);
        let (head, tail) = b.config_json_around_devices();
        let devices: Vec<String> = (0..b.device_count()).filter_map(|k| b.device_json(k)).collect();
        let pieced = format!("{head}{}{tail}", devices.join(","));
        let a: Value = serde_json::from_str(&pieced).unwrap();
        let whole: Value = serde_json::from_str(&b.config_json()).unwrap();
        assert_eq!(a, whole);
    }

    #[test]
    fn heating_settings_out_of_range_are_refused() {
        let mut b = bridge();
        b.configure_bridge(&json!({"heating": {"min_on_minutes": 1000000}}), ctx(0));
        b.configure_bridge(&json!({"heating": {"cycle_minutes": 0}}), ctx(0));
        b.configure_bridge(&json!({"heating": {"min_valve_position": 200}}), ctx(0));
        assert_eq!(b.config().heating, HeatingConfig::default());
        b.configure_bridge(&json!({"heating": {"cycle_minutes": 15}}), ctx(0));
        assert_eq!(b.config().heating.cycle_minutes, 15.0);
        // A stored config with nonsense falls back to the defaults.
        let mut c = b.config();
        c.heating.min_on_minutes = f32::NAN;
        assert_eq!(Bridge::new(c, Topics::default(), 0).config().heating, HeatingConfig::default());
    }

    #[test]
    fn ack_of_an_older_group_does_not_confirm_the_newer_one() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        b.configure_device(HT, &json!({"group": 3}), ctx(1)).unwrap();
        let Next::Send { id, bytes, .. } = b.next_tx(ctx(1)) else { panic!() };
        b.transmitted(id, 1);
        // Changed again before the device answered the first.
        b.configure_device(HT, &json!({"group": 6}), ctx(2)).unwrap();
        let mut raw = bytes.clone();
        raw.extend_from_slice(&[0, 0x80]);
        let f = decode_rx(&raw).unwrap().frame;
        assert_eq!(f.payload, payload::set_group(3));
        let mut events = Vec::new();
        b.tx.ack(HT, f.msgcnt, true, &mut events);
        b.log_events(events);
        let i = b.find(HT).unwrap();
        assert!(b.devices[i].cfg.pending.contains(&Pending::Group), "group 6 still to confirm");
        let s = sent(&drain(&mut b, 3));
        assert!(s.contains(&(HT, command::SET_GROUP_ID, payload::set_group(6))));
        assert!(b.devices[i].cfg.pending.is_empty());
    }

    #[test]
    fn device_talking_to_a_stranger_is_told_to_drop_it() {
        let mut b = bridge();
        living_room(&mut b, 0);
        // A radiator elsewhere, known to the bridge.
        b.handle_radio(&rx(NEIGHBOUR, ME, 1, command::THERMOSTAT_STATE, vec![0x01, 0, 42, 0, 200]), ctx(0));
        drain(&mut b, 0);
        // The living room radiator still wakes it up, from an old link.
        let wake = rx(HT, NEIGHBOUR, 7, command::WAKE_UP, vec![0x03]);
        b.handle_radio(&wake, ctx(1_000));
        let s = sent(&drain(&mut b, 1_000));
        let unlink = payload::link_partner(NEIGHBOUR, DeviceType::HeatingThermostat);
        assert_eq!(s, vec![(HT, command::REMOVE_LINK_PARTNER, unlink)]);
        // Once: not again on its next try, nor for its real partner.
        b.handle_radio(&wake, ctx(2_000));
        b.handle_radio(&rx(WT, HT, 8, command::WALL_THERMOSTAT_CONTROL, vec![44, 210]), ctx(3_000));
        assert!(drain(&mut b, 3_000).iter().all(|f| f.command != command::REMOVE_LINK_PARTNER));
    }

    #[test]
    fn device_using_another_group_gets_its_own_again() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        let hi = b.find(HT).unwrap();
        let mine = b.devices[hi].cfg.group;
        assert_ne!(mine, 0);
        // The radiator's own message, with another room's group in the header.
        let mut raw = Frame {
            msgcnt: 9,
            flags: FLAG_GROUP,
            command: command::THERMOSTAT_STATE,
            src: HT,
            dst: Address([0; 3]),
            group: mine + 3,
            payload: vec![0x01, 0, 42, 0, 200],
        }
        .encode();
        raw.extend_from_slice(&[0x10, 0x80]);
        b.handle_radio(&raw, ctx(1_000));
        let s = sent(&drain(&mut b, 1_000));
        assert_eq!(s, vec![(HT, command::SET_GROUP_ID, payload::set_group(mine))]);
        // Once since start, so a device that keeps the wrong one is not flooded.
        b.handle_radio(&raw, ctx(2_000));
        assert!(drain(&mut b, 2_000).is_empty());
    }

    #[test]
    fn factory_reset_over_the_radio() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        b.configure_device(HT, &json!({"factory_reset": true}), ctx(1)).unwrap();
        assert_eq!(sent(&drain(&mut b, 1)), vec![(HT, command::RESET, vec![])]);
        // Still known, room and all, for pairing it again.
        assert!(b.find(HT).is_some_and(|i| b.devices[i].cfg.room.is_some()));
    }

    #[test]
    fn rejected_unlink_counts_as_done() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        b.configure_device(WT, &json!({"unlink": {"address": "0FB714", "type": "shutter_contact"}}), ctx(1)).unwrap();
        let Next::Send { id, bytes, .. } = b.next_tx(ctx(1)) else { panic!() };
        b.transmitted(id, 1);
        let mut raw = bytes.clone();
        raw.extend_from_slice(&[0, 0x80]);
        let f = decode_rx(&raw).unwrap().frame;
        let mut events = Vec::new();
        b.tx.ack(WT, f.msgcnt, false, &mut events);
        b.log_events(events);
        assert!(b.devices[b.find(WT).unwrap()].cfg.pending.is_empty(), "no such link: nothing to retry");
    }

    #[test]
    fn rejected_settings_stay_unconfirmed() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        b.configure_device(HT, &json!({"comfort_temperature": 22}), ctx(1_000)).unwrap();
        let f = match b.next_tx(ctx(1_000)) {
            Next::Send { id, bytes, .. } => {
                b.transmitted(id, 1_000);
                let mut raw = bytes.clone();
                raw.extend_from_slice(&[0, 0x80]);
                decode_rx(&raw).unwrap().frame
            }
            other => panic!("{other:?}"),
        };
        assert_eq!(f.command, command::CONFIG_TEMPERATURES);
        let mut events = Vec::new();
        b.tx.ack(HT, f.msgcnt, false, &mut events);
        b.log_events(events);
        let i = b.find(HT).unwrap();
        assert!(b.devices[i].cfg.pending.contains(&Pending::Temperatures));
    }

    #[test]
    fn descaling_valve_is_ignored() {
        let mut b = bridge();
        living_room(&mut b, 0);
        // Decalc: Saturday 12:00 local by default; ctx() is Monday 2026-11-16 09:00 CET.
        let saturday_noon =
            Ctx { now: 0, unix_time: Some((days_from_civil(2026, 11, 21) * 86_400) as u64 + 11 * 3600) };
        b.handle_radio(&rx(HT, ME, 2, command::THERMOSTAT_STATE, vec![0x01, 100, 42, 0, 200]), saturday_noon);
        assert_eq!(b.demand(saturday_noon).output, 0.0);
        assert_eq!(b.demand(ctx(0)).output, 100.0);
    }

    #[test]
    fn boost_heats() {
        let mut b = bridge();
        living_room(&mut b, 0);
        b.handle_radio(&rx(WT, ME, 2, command::WALL_THERMOSTAT_STATE, vec![0x03, 0, 42, 0, 0, 0, 215]), ctx(1_000));
        assert!(b.demand(ctx(1_000)).boost);
        assert!(!b.demand(ctx(29 * MINUTE)).boost);
    }

    #[test]
    fn config_is_saved_after_delay_and_roundtrips() {
        let mut b = bridge();
        living_room(&mut b, 0);
        assert!(!b.tick(ctx(1_000)).contains(&Effect::SaveConfig));
        assert!(b.tick(ctx(SAVE_DELAY)).contains(&Effect::SaveConfig));
        assert!(!b.tick(ctx(SAVE_DELAY + 1_000)).contains(&Effect::SaveConfig));
        let json = b.config_json();
        let c: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(c.devices.len(), 2);
        assert_eq!(c.devices[0].room.as_deref(), Some("living-room"));
        let b2 = Bridge::new(c, Topics::default(), 0);
        assert_eq!(b2.devices()[1].cfg.name, "living-room/heater");
    }

    #[test]
    fn retained_reset_is_ignored() {
        let mut b = bridge();
        assert!(b.handle_mqtt("max/reset", b"", true, ctx(0)).is_empty());
        assert_eq!(b.handle_mqtt("max/reset", b"", false, ctx(0)), vec![Effect::Restart]);
    }

    #[test]
    fn bridge_settings_merge() {
        let mut b = bridge();
        // `kp` is from the old PID: ignored, like any unknown key.
        let out =
            web(&mut b, "max/set", json!({"heating": {"min_valve_position": 20, "kp": 30}, "autocreate": false}), 0);
        assert_eq!(b.settings.heating.min_valve_position, 20);
        assert_eq!(b.settings.heating.cycle_minutes, 20.0);
        assert!(!b.settings.autocreate);
        assert_eq!(publishes(&out, "max")[0]["autocreate"], false);
        b.handle_radio(&rx(NEIGHBOUR, ME, 1, command::ACK, vec![1]), ctx(0));
        assert!(b.devices.is_empty(), "autocreate off");
    }

    #[test]
    fn reconnect_republishes_only_heard_devices() {
        let mut c = Config::default();
        c.devices.push(DeviceConfig { address: HT, name: "heater".into(), ..Default::default() });
        c.devices.push(DeviceConfig { address: WT, name: "thermostat".into(), ..Default::default() });
        let mut b = Bridge::new(c, Topics::default(), 0);
        b.handle_radio(&rx(HT, ME, 1, command::THERMOSTAT_STATE, vec![0x01, 40, 43, 0, 212]), ctx(0));
        let out = connect(&mut b);
        assert!(out.contains(&Effect::Subscribe("max/heater/set".into())));
        assert!(out.contains(&Effect::Subscribe("max/thermostat/set".into())));
        assert_eq!(publishes(&out, "max/heater").len(), 1);
        assert!(publishes(&out, "max/thermostat").is_empty());
        assert_eq!(publishes(&out, "max")[0]["availability"], "online");
    }

    #[test]
    fn hourly_time_sync_reaches_each_thermostat() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        let mut synced = BTreeSet::new();
        for hour in 1..=6 {
            let now = hour * TIME_SYNC_INTERVAL;
            b.last_rx = now;
            b.tick(ctx(now));
            for f in drain(&mut b, now) {
                assert_eq!(f.command, command::TIME_INFORMATION);
                synced.insert(f.dst);
            }
        }
        assert_eq!(synced, [WT, HT].into_iter().collect());
    }

    #[test]
    fn silent_valve_is_asked_for_its_state_before_it_goes_stale() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        let asked = |b: &mut Bridge, now: Millis| {
            b.last_rx = now;
            b.tick(ctx(now));
            drain(b, now).iter().filter(|f| f.command == command::TIME_INFORMATION && f.dst == HT).count()
        };
        // Reporting by itself: never asked.
        for m in 1..=7 {
            assert_eq!(asked(&mut b, m * MINUTE), 0);
            b.handle_radio(
                &rx(HT, ME, m as u8 + 1, command::THERMOSTAT_STATE, vec![0x01, 50, 42, 0, 200]),
                ctx(m * MINUTE),
            );
        }
        // Silent for 8 of the 10 minutes: asked once, and the ACK keeps the reading fresh.
        let last = 7 * MINUTE;
        assert_eq!(asked(&mut b, last + 8 * MINUTE - 1_000), 0);
        assert_eq!(asked(&mut b, last + 8 * MINUTE), 1);
        assert_eq!(asked(&mut b, last + 8 * MINUTE + 1_000), 0, "not again while waiting");
        b.handle_radio(&rx(HT, ME, 20, command::ACK, vec![0x01, 0x01, 30, 42]), ctx(last + 8 * MINUTE + 2_000));
        let answered = last + 8 * MINUTE + 2_000;
        assert_eq!(b.demand(ctx(answered + 9 * MINUTE)).output, valve_demand(&b.settings.heating, 30));
        // Silent again: asked again. No answer: asked again only after ASK_AGAIN.
        let second = answered + 8 * MINUTE;
        assert_eq!(asked(&mut b, second), 1);
        assert_eq!(asked(&mut b, second + ASK_AGAIN - 1_000), 0);
        assert_eq!(asked(&mut b, second + ASK_AGAIN), 1);
    }

    #[test]
    fn valves_not_heard_since_start_are_asked() {
        let mut b = bridge();
        living_room(&mut b, 0);
        b.devices.iter_mut().for_each(|d| d.rt = Runtime::default());
        drain(&mut b, 0);
        b.last_rx = 8 * MINUTE;
        b.tick(ctx(8 * MINUTE));
        let sent = drain(&mut b, 8 * MINUTE);
        assert!(sent.iter().any(|f| f.command == command::TIME_INFORMATION && f.dst == HT), "{sent:?}");
        assert!(!sent.iter().any(|f| f.dst == WT), "only radiator valves are asked");
    }

    #[test]
    fn radio_watchdog() {
        let mut b = bridge();
        assert!(!b.tick(ctx(4 * MINUTE)).contains(&Effect::ReinitRadio));
        assert!(b.tick(ctx(5 * MINUTE)).contains(&Effect::ReinitRadio));
        assert!(!b.tick(ctx(6 * MINUTE)).contains(&Effect::ReinitRadio));
    }

    fn ha_bridge(prefix: &str) -> Bridge {
        let topics = Topics { prefix: prefix.into(), discovery: Some("homeassistant".into()) };
        Bridge::new(Config { duty_cycle_limit: false, ..Config::default() }, topics, 0)
    }

    fn discovery_publishes(out: &[Effect]) -> Vec<(String, String)> {
        out.iter()
            .filter_map(|e| match e {
                Effect::Publish { topic, payload, retain: true } if topic.starts_with("homeassistant/") => {
                    Some((topic.clone(), payload.clone()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn custom_prefix() {
        let topics = Topics { prefix: "max-test".into(), discovery: None };
        let mut b = Bridge::new(Config::default(), topics, 0);
        let out = b.handle_radio(&rx(HT, ME, 3, command::THERMOSTAT_STATE, vec![0x01, 40, 43, 0, 212]), ctx(0));
        assert!(out.contains(&Effect::Subscribe("max-test/0B0002/set".into())), "{out:?}");
        assert!(!publishes(&out, "max-test/0B0002").is_empty());
        // The default prefix is not ours any more.
        assert!(mqtt(&mut b, "max/reset", json!({}), 0).is_empty());
        assert_eq!(b.handle_mqtt("max-test/reset", b"", false, ctx(0)), vec![Effect::Restart]);
    }

    #[test]
    fn announces_devices_with_a_room_only() {
        let mut b = ha_bridge("max");
        let out = b.handle_radio(&rx(HT, ME, 3, command::THERMOSTAT_STATE, vec![0x01, 40, 43, 0, 212]), ctx(0));
        assert!(discovery_publishes(&out).is_empty(), "a neighbour's valve is not announced");

        let out = web(&mut b, "max/0B0002/set", json!({"room": "kitchen"}), 0);
        let d = discovery_publishes(&out);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].0, "homeassistant/device/max2mqtt_0b0002/config");
        assert!(d[0].1.contains("\"sa\":\"kitchen\""), "{}", d[0].1);

        // Unchanged: no new announcement.
        let out = b.handle_radio(&rx(HT, ME, 4, command::THERMOSTAT_STATE, vec![0x01, 40, 43, 0, 212]), ctx(1000));
        assert!(discovery_publishes(&out).is_empty());

        // Rename: announced again with the new topics.
        let out = web(&mut b, "max/rename", json!({"address": "0B0002", "to": "kitchen/heater"}), 2000);
        let d = discovery_publishes(&out);
        assert_eq!(d.len(), 1);
        assert!(d[0].1.contains("\"stat_t\":\"max/kitchen/heater\""), "{}", d[0].1);

        // No room: removed.
        let out = web(&mut b, "max/kitchen/heater/set", json!({"room": ""}), 3000);
        assert_eq!(
            discovery_publishes(&out),
            vec![("homeassistant/device/max2mqtt_0b0002/config".into(), String::new())]
        );
    }

    #[test]
    fn home_assistant_restart_gets_everything_again() {
        let mut b = ha_bridge("max");
        living_room(&mut b, 0);
        // The first step holds only the bridge's own messages; devices follow one by one.
        let first = b.on_mqtt_connected();
        assert_eq!(discovery_publishes(&first).len(), 1);
        assert_eq!(b.next_announcement().map(|e| discovery_publishes(&e).len()), Some(1));
        b.replay = None;
        let connected = connect(&mut b);
        assert!(connected.contains(&Effect::Subscribe("homeassistant/status".into())));
        // Bridge plus the two living room devices.
        assert_eq!(discovery_publishes(&connected).len(), 3, "{connected:?}");
        let mut again = b.handle_mqtt("homeassistant/status", b"online", false, ctx(0));
        while let Some(more) = b.next_announcement() {
            again.extend(more);
        }
        assert_eq!(discovery_publishes(&again).len(), 3);
        assert!(b.handle_mqtt("homeassistant/status", b"offline", false, ctx(0)).is_empty());
    }

    #[test]
    fn configure_device_from_web() {
        let mut b = bridge();
        b.handle_radio(&rx(HT, ME, 3, command::THERMOSTAT_STATE, vec![0x01, 40, 43, 0, 212]), ctx(0));
        let out = b.configure_device(HT, &json!({"name": "bath/heater", "room": "bath"}), ctx(0)).unwrap();
        assert!(out.contains(&Effect::Subscribe("max/bath/heater/set".into())));
        assert_eq!(b.devices[0].cfg.room.as_deref(), Some("bath"));
        assert!(b.configure_device(HT, &json!({"name": "set"}), ctx(0)).is_err());
        assert!(b.configure_device(NEIGHBOUR, &json!({}), ctx(0)).is_err());
        let status = serde_json::to_value(b.status(ctx(5000))).unwrap();
        assert_eq!(status["devices"][0]["name"], "bath/heater");
        assert_eq!(status["devices"][0]["seen_seconds_ago"], 5);
    }

    const WINDOW: Address = Address([0x0D, 0x00, 0x04]);

    /// (destination, command, payload) of each frame, for comparing.
    fn sent(frames: &[Frame]) -> Vec<(Address, u8, Vec<u8>)> {
        frames.iter().map(|f| (f.dst, f.command, f.payload.clone())).collect()
    }

    #[test]
    fn room_links_its_thermostats_and_shares_a_group() {
        let mut b = bridge();
        living_room(&mut b, 0);
        let s = sent(&drain(&mut b, 0));
        assert!(s.contains(&(WT, command::SET_GROUP_ID, vec![1])), "{s:02x?}");
        assert!(s.contains(&(HT, command::SET_GROUP_ID, vec![1])));
        assert!(s.contains(&(WT, command::ADD_LINK_PARTNER, payload::link_partner(HT, DeviceType::HeatingThermostat))));
        assert!(s.contains(&(HT, command::ADD_LINK_PARTNER, payload::link_partner(WT, DeviceType::WallThermostat))));
        // Nothing more once in sync.
        web(&mut b, "max/living-room/heater/set", json!({"room": "living-room"}), 1);
        assert!(drain(&mut b, 1).is_empty());

        // A second room gets the next group.
        b.handle_radio(&rx(NEIGHBOUR, ME, 1, command::THERMOSTAT_STATE, vec![0x01, 0, 40, 0, 200]), ctx(2));
        web(&mut b, "max/0C0003/set", json!({"room": "kitchen"}), 2);
        assert_eq!(sent(&drain(&mut b, 2)), vec![(NEIGHBOUR, command::SET_GROUP_ID, vec![2])]);
    }

    #[test]
    fn leaving_a_room_unlinks() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        web(&mut b, "max/living-room/heater/set", json!({"room": ""}), 1);
        let s = sent(&drain(&mut b, 1));
        assert!(s.contains(&(HT, command::REMOVE_GROUP_ID, vec![0])), "{s:02x?}");
        assert!(s.contains(&(HT, command::REMOVE_LINK_PARTNER, payload::link_partner(WT, DeviceType::WallThermostat))));
        assert!(s.contains(&(
            WT,
            command::REMOVE_LINK_PARTNER,
            payload::link_partner(HT, DeviceType::HeatingThermostat)
        )));
        assert!(b.devices.iter().all(|d| d.cfg.associations.is_empty()));
    }

    #[test]
    fn window_sensor_links_wait_until_it_wakes() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        b.handle_radio(&rx(WINDOW, ME, 1, command::SHUTTER_CONTACT_STATE, vec![0x10]), ctx(1));
        drain(&mut b, 1);
        b.configure_device(WINDOW, &json!({"room": "living-room"}), ctx(2)).unwrap();
        let s = sent(&drain(&mut b, 2));
        // The thermostats get theirs now; the sleeping sensor's wait.
        assert!(s.contains(&(
            HT,
            command::ADD_LINK_PARTNER,
            payload::link_partner(WINDOW, DeviceType::ShutterContact)
        )));
        assert!(s.contains(&(
            WT,
            command::ADD_LINK_PARTNER,
            payload::link_partner(WINDOW, DeviceType::ShutterContact)
        )));
        assert!(s.iter().all(|(dst, ..)| *dst != WINDOW), "{s:02x?}");
        assert_eq!(b.tx.parked(), 2);
        // Window opened: it is awake.
        b.handle_radio(&rx(WINDOW, ME, 2, command::SHUTTER_CONTACT_STATE, vec![0x12]), ctx(3));
        let s = sent(&drain(&mut b, 3));
        assert!(s.contains(&(
            WINDOW,
            command::ADD_LINK_PARTNER,
            payload::link_partner(HT, DeviceType::HeatingThermostat)
        )));
        assert!(s.contains(&(
            WINDOW,
            command::ADD_LINK_PARTNER,
            payload::link_partner(WT, DeviceType::WallThermostat)
        )));
        assert_eq!(b.tx.parked(), 0);
    }

    #[test]
    fn relink_sends_everything_again() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        b.configure_device(HT, &json!({"relink": true}), ctx(1)).unwrap();
        let s = sent(&drain(&mut b, 1));
        let count = |cmd| s.iter().filter(|(_, c, _)| *c == cmd).count();
        assert_eq!(count(command::SET_GROUP_ID), 2, "{s:02x?}");
        // Each link once, although both devices list it.
        assert_eq!(count(command::ADD_LINK_PARTNER), 2);
        assert_eq!(count(command::CONFIG_TEMPERATURES), 2);
        assert_eq!(count(command::TIME_INFORMATION), 2);
    }

    #[test]
    fn unlink_from_a_forgotten_partner_is_retried() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        b.forget_device(HT, ctx(1)).unwrap();
        let wt = b.find(WT).unwrap();
        let unlink = Pending::Unlinked(HT, DeviceType::HeatingThermostat);
        assert!(b.devices[wt].cfg.pending.contains(&unlink), "{:?}", b.devices[wt].cfg.pending);
        // The first try is lost; the resend still knows what to say, with the partner gone.
        b.tx = TxQueue::new(false, 1);
        b.send_pending(wt, 2);
        let s = sent(&drain(&mut b, 2));
        assert!(s.contains(&(
            WT,
            command::REMOVE_LINK_PARTNER,
            payload::link_partner(HT, DeviceType::HeatingThermostat)
        )));
        assert!(!b.devices[wt].cfg.pending.contains(&unlink), "confirmed by the ACK");
    }

    #[test]
    fn unlink_reaches_a_partner_the_bridge_forgot() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        let gone = Address([0x0F, 0xB7, 0x14]);
        b.configure_device(WT, &json!({"unlink": {"address": "0FB714", "type": "shutter_contact"}}), ctx(1)).unwrap();
        let s = sent(&drain(&mut b, 1));
        assert!(s.contains(&(
            WT,
            command::REMOVE_LINK_PARTNER,
            payload::link_partner(gone, DeviceType::ShutterContact)
        )));
        b.configure_device(WT, &json!({"unlink": {"address": "0FB714"}}), ctx(2)).unwrap();
        assert!(drain(&mut b, 2).is_empty(), "without a type nothing is sent");
    }

    #[test]
    fn room_schedule_goes_to_every_thermostat_in_the_room() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        let week = json!({"monday": {"6:00": 21.5, "22:30": 17}, "sunday": {"8:00": 21}});
        b.configure_device(HT, &json!({"room_schedule": week}), ctx(1)).unwrap();
        let s = sent(&drain(&mut b, 1));
        for dst in [WT, HT] {
            // One message per day (a second one only for days with more than 7 points).
            assert_eq!(
                s.iter().filter(|(d, c, _)| *d == dst && *c == command::CONFIG_WEEK_PROFILE).count(),
                2,
                "{s:02x?}"
            );
        }
        assert_eq!(b.devices[0].cfg.schedule, b.devices[1].cfg.schedule);
        // Invalid: refused up front, nothing sent.
        let err = b.configure_device(HT, &json!({"room_schedule": {"monday": {"6:03": 21}}}), ctx(2)).unwrap_err();
        assert!(err.contains("monday"), "{err}");
        assert!(drain(&mut b, 2).is_empty());
    }

    #[test]
    fn newcomer_takes_over_the_room_program() {
        let mut b = bridge();
        living_room(&mut b, 0);
        b.configure_device(HT, &json!({"room_schedule": {"monday": {"6:00": 21}}}), ctx(1)).unwrap();
        drain(&mut b, 1);
        b.handle_radio(&rx(NEIGHBOUR, ME, 1, command::THERMOSTAT_STATE, vec![0x01, 0, 40, 0, 200]), ctx(2));
        b.configure_device(NEIGHBOUR, &json!({"room": "living-room"}), ctx(2)).unwrap();
        let s = sent(&drain(&mut b, 2));
        assert_eq!(s.iter().filter(|(d, c, _)| *d == NEIGHBOUR && *c == command::CONFIG_WEEK_PROFILE).count(), 1);
    }

    #[test]
    fn time_zone_change_resets_thermostat_clocks() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        web(&mut b, "max/set", json!({"timezone": {"utc_offset_minutes": 120}}), 1);
        let s = sent(&drain(&mut b, 1));
        let times: Vec<_> = s.iter().filter(|(_, c, _)| *c == command::TIME_INFORMATION).collect();
        assert_eq!(times.len(), 2, "{s:02x?}");
        // 08:00 UTC is now 10:00.
        assert_eq!(times[0].2[2], 10);
        // Same zone again: nothing.
        web(&mut b, "max/set", json!({"timezone": {"utc_offset_minutes": 120}}), 2);
        assert!(drain(&mut b, 2).is_empty());
    }

    #[test]
    fn mqtt_only_controls() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        // Configuration over MQTT is ignored...
        mqtt(&mut b, "max/living-room/heater/set", json!({"room": "", "eco_temperature": 10}), 1);
        assert_eq!(b.devices[1].cfg.room.as_deref(), Some("living-room"));
        assert!(drain(&mut b, 1).is_empty());
        mqtt(&mut b, "max/set", json!({"address": "ABCDEF", "heating": {"cycle_minutes": 5}}), 1);
        assert_eq!(b.address(), ME);
        assert!(b.handle_mqtt("max/format", b"", false, ctx(1)).is_empty());
        assert!(connect(&mut b).iter().all(|e| !matches!(e, Effect::Subscribe(t) if t.contains("rename"))));
        // ...while what Home Assistant sends still works, also next to an ignored key.
        mqtt(&mut b, "max/living-room/heater/set", json!({"temperature": 22.5, "room": "x"}), 2);
        let s = sent(&drain(&mut b, 2));
        assert!(s.iter().any(|(d, c, _)| *d == HT && *c == command::SET_TEMPERATURE), "{s:02x?}");
        mqtt(&mut b, "max/set", json!({"pair": true}), 3);
        assert!(b.pairing_until.is_some());
    }

    #[test]
    fn type_can_be_set_for_devices_only_heard_in_acks() {
        let mut b = bridge();
        living_room(&mut b, 0);
        b.handle_radio(&rx(NEIGHBOUR, WT, 1, command::ACK, vec![0x01, 0x01, 60, 42]), ctx(1));
        assert_eq!(b.devices[2].cfg.device_type, None);
        drain(&mut b, 1);
        b.configure_device(NEIGHBOUR, &json!({"room": "living-room", "type": "heater"}), ctx(2)).unwrap();
        let s = sent(&drain(&mut b, 2));
        assert!(
            s.contains(&(NEIGHBOUR, command::ADD_LINK_PARTNER, payload::link_partner(WT, DeviceType::WallThermostat))),
            "{s:02x?}"
        );
        assert!(b.configure_device(NEIGHBOUR, &json!({"type": "toaster"}), ctx(3)).is_ok());
    }

    #[test]
    fn device_can_be_added_before_it_is_heard() {
        let mut b = bridge();
        let out = b
            .configure_device(
                NEIGHBOUR,
                &json!({"name": "hallway/heater", "room": "hallway", "type": "heater"}),
                ctx(0),
            )
            .unwrap();
        assert!(out.contains(&Effect::Subscribe("max/hallway/heater/set".into())));
        assert_eq!(b.devices[0].cfg.room.as_deref(), Some("hallway"));
        assert!(b.configure_device(WT, &json!({"room": "x"}), ctx(0)).is_err(), "only with a name");
        assert!(b.configure_device(ME, &json!({"name": "me"}), ctx(0)).is_err());
    }

    /// Send everything queued, letting time pass, with nobody answering, until the queue
    /// gives up.
    fn drain_unacked(b: &mut Bridge, mut now: Millis) -> Vec<Frame> {
        let mut sent = Vec::new();
        for _ in 0..500 {
            match b.next_tx(ctx(now)) {
                Next::Send { id, bytes, .. } => {
                    b.transmitted(id, now);
                    let mut raw = bytes.clone();
                    raw.extend_from_slice(&[0, 0x80]);
                    sent.push(decode_rx(&raw).unwrap().frame);
                }
                Next::WaitUntil(t) => now = t,
                Next::Idle => break,
            }
        }
        sent
    }

    #[test]
    fn settings_stay_pending_until_acknowledged() {
        let mut b = bridge();
        living_room(&mut b, 0);
        let p = &b.devices[1].cfg.pending;
        assert!(p.contains(&Pending::Group) && p.contains(&Pending::Link(WT)), "{p:?}");
        drain(&mut b, 0);
        assert!(b.devices.iter().all(|d| d.cfg.pending.is_empty()));
    }

    #[test]
    fn unconfirmed_settings_survive_a_restart_and_go_again_when_heard() {
        let mut b = bridge();
        living_room(&mut b, 0);
        b.configure_device(HT, &json!({"comfort_temperature": 22}), ctx(1)).unwrap();
        // Restart before anything went out: the queue is lost, the pending set is stored.
        let config = b.config();
        let mut b = Bridge::new(config, Topics::default(), 0);
        assert!(b.devices[1].cfg.pending.contains(&Pending::Temperatures));
        assert!(drain(&mut b, 0).is_empty(), "nothing is sent blindly at boot");
        b.handle_radio(&rx(HT, ME, 2, command::THERMOSTAT_STATE, vec![0x01, 50, 42, 0, 200]), ctx(1_000));
        let s = sent(&drain(&mut b, 1_000));
        assert!(
            s.contains(&(
                HT,
                command::CONFIG_TEMPERATURES,
                payload::config_temperatures(22.0, 17.0, 30.5, 4.5, 0.0, 4.5, 15)
            )),
            "{s:02x?}"
        );
        assert!(s.iter().all(|(d, ..)| *d == HT), "only the device that was heard");
        assert!(b.devices[1].cfg.pending.is_empty());
    }

    #[test]
    fn a_device_that_does_not_answer_is_retried_at_most_every_15_minutes() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain_unacked(&mut b, 0);
        let heard = |b: &mut Bridge, now| {
            b.handle_radio(&rx(HT, ME, 2, command::THERMOSTAT_STATE, vec![0x01, 50, 42, 0, 200]), ctx(now));
            !drain_unacked(b, now).is_empty()
        };
        assert!(heard(&mut b, MINUTE));
        assert!(!heard(&mut b, 10 * MINUTE));
        assert!(heard(&mut b, 17 * MINUTE));
    }

    #[test]
    fn config_from_before_tracking_is_sent_once() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        let mut old = b.config();
        old.tracking = false;
        let mut b = Bridge::new(old, Topics::default(), 0);
        let p = &b.devices[1].cfg.pending;
        assert!(p.contains(&Pending::Temperatures) && p.contains(&Pending::Link(WT)) && p.contains(&Pending::Group));
        assert!(b.config().tracking);
        b.handle_radio(&rx(HT, ME, 2, command::THERMOSTAT_STATE, vec![0x01, 50, 42, 0, 200]), ctx(1_000));
        assert!(!drain(&mut b, 1_000).is_empty());
        assert!(b.devices[1].cfg.pending.is_empty());
    }

    #[test]
    fn long_week_day_is_confirmed_by_its_second_message() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        let day: serde_json::Map<String, Value> =
            (0..9).map(|h| (format!("{:02}:00", h * 2 + 4), json!(18 + h % 3))).collect();
        b.configure_device(HT, &json!({"room_schedule": {"monday": day}}), ctx(1)).unwrap();
        assert_eq!(b.program_parts(1, Weekday::Monday), 2);
        // The first part alone does not confirm the day.
        b.confirm(HT, command::CONFIG_WEEK_PROFILE, &[Weekday::Monday.max_index()]);
        assert!(b.devices[1].cfg.pending.contains(&Pending::Program(Weekday::Monday)));
        b.confirm(HT, command::CONFIG_WEEK_PROFILE, &[0x10 | Weekday::Monday.max_index()]);
        assert!(!b.devices[1].cfg.pending.contains(&Pending::Program(Weekday::Monday)));
    }

    #[test]
    fn forgetting_a_device_unlinks_it_everywhere() {
        let mut b = ha_bridge("max");
        living_room(&mut b, 0);
        drain(&mut b, 0);
        let out = b.forget_device(HT, ctx(1)).unwrap();
        assert!(out.contains(&Effect::Publish {
            topic: "homeassistant/device/max2mqtt_0b0002/config".into(),
            payload: String::new(),
            retain: true
        }));
        assert!(out.contains(&Effect::Publish {
            topic: "max/living-room/heater".into(),
            payload: String::new(),
            retain: true
        }));
        let s = sent(&drain(&mut b, 1));
        assert_eq!(
            s,
            vec![(WT, command::REMOVE_LINK_PARTNER, payload::link_partner(HT, DeviceType::HeatingThermostat))]
        );
        assert_eq!(b.devices.len(), 1);
        assert!(b.devices[0].cfg.associations.is_empty());
    }

    #[test]
    fn window_sensors_get_no_time() {
        let mut b = bridge();
        living_room(&mut b, 0);
        drain(&mut b, 0);
        b.handle_radio(&rx(WINDOW, ME, 1, command::SHUTTER_CONTACT_STATE, vec![0x10]), ctx(1));
        b.configure_device(WINDOW, &json!({"room": "living-room", "relink": true}), ctx(2)).unwrap();
        b.handle_radio(&rx(WINDOW, ME, 2, command::SHUTTER_CONTACT_STATE, vec![0x12]), ctx(3));
        let s = sent(&drain(&mut b, 3));
        assert!(!s.iter().any(|(d, c, _)| *d == WINDOW && *c == command::TIME_INFORMATION), "{s:02x?}");
    }

    #[test]
    fn thermostats_get_unconfirmed_settings_without_being_heard() {
        let mut b = bridge();
        living_room(&mut b, 0);
        b.configure_device(HT, &json!({"comfort_temperature": 22}), ctx(1)).unwrap();
        let mut b = Bridge::new(b.config(), Topics::default(), 0);
        // One device per tick: the wall thermostat first, then the valve.
        b.tick(ctx(1_000));
        b.tick(ctx(2_000));
        let s = sent(&drain(&mut b, 2_000));
        assert!(s.iter().any(|(d, c, _)| *d == HT && *c == command::CONFIG_TEMPERATURES), "{s:02x?}");
        // Not again within 15 minutes, even if still unconfirmed.
        b.devices[1].cfg.pending.insert(Pending::Valve);
        b.tick(ctx(3_000));
        assert!(drain(&mut b, 3_000).iter().all(|f| f.dst != HT));
    }
}
