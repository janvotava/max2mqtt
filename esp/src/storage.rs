//! Persistent storage in the `config` data partition (see `partitions.csv`), as a
//! [`sequential_storage`] key-value map: wear-levelled, power-fail safe, CRC-checked.
//!
//! Keys are four bytes, a tag and a device address:
//! - `[0, 0, 0, 0]`: device [`Settings`] (network, MQTT, radio), JSON.
//! - `[1, 0, 0, 0]`: bridge [`Config`] without its devices, JSON.
//! - `[2, a, b, c]`: one MAX! device, a [`DeviceConfig`] as JSON.
//! - `[3, 0, 0, 0]`: trial boots of a new firmware image, one byte.
//! - `[4, 0, 0, 0]`: the last crashes, see [`crate::crash`], JSON.
//!
//! One entry per device keeps every entry well inside a 4 KiB flash page, and a change to
//! one device rewrites only that device.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_storage::nor_flash as blocking;
use embedded_storage_async::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use esp_bootloader_esp_idf::partitions::{self, PARTITION_TABLE_MAX_LEN};
use esp_storage::{FlashStorage, FlashStorageError};
use max2mqtt_bridge::config::DeviceConfig;
use max2mqtt_bridge::{Address, Config, Settings};
use sequential_storage::cache::{Cache, Uncached};
use sequential_storage::map::{MapConfig, MapStorage};

const CONFIG_LABEL: &str = "config";
// Labelled spiffs (0x82) so every tool accepts the table; the content is our own format.
const CONFIG_SUBTYPE: u8 = 0x82;
/// Largest entry: one flash page minus the map's overhead.
const BUFFER: usize = 4096;

type Key = [u8; 4];
const SETTINGS: Key = [0, 0, 0, 0];
const BRIDGE: Key = [1, 0, 0, 0];
const DEVICE: u8 = 2;
/// Boots of a new firmware image that has not been confirmed yet (see `ota`).
const TRIAL_BOOTS: Key = [3, 0, 0, 0];
const CRASHES: Key = [4, 0, 0, 0];

fn device_key(a: Address) -> Key {
    [DEVICE, a.0[0], a.0[1], a.0[2]]
}

/// The blocking ESP flash driver behind the async traits sequential-storage wants. Flash
/// operations stall the CPU anyway (the cache is off while writing).
pub struct Flash(FlashStorage<'static>);

impl ErrorType for Flash {
    type Error = FlashStorageError;
}

impl ReadNorFlash for Flash {
    const READ_SIZE: usize = <FlashStorage as blocking::ReadNorFlash>::READ_SIZE;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        blocking::ReadNorFlash::read(&mut self.0, offset, bytes)
    }

    fn capacity(&self) -> usize {
        blocking::ReadNorFlash::capacity(&self.0)
    }
}

impl NorFlash for Flash {
    const WRITE_SIZE: usize = <FlashStorage as blocking::NorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <FlashStorage as blocking::NorFlash>::ERASE_SIZE;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        blocking::NorFlash::erase(&mut self.0, from, to)
    }

    // NOR semantics (only clears bits). FlashStorage's inherent `write` would erase the
    // sector around it first, which breaks the map.
    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        blocking::NorFlash::write(&mut self.0, offset, bytes)
    }
}

impl MultiwriteNorFlash for Flash {}

pub type Error = sequential_storage::Error<FlashStorageError>;

pub struct Store {
    map: MapStorage<Key, Flash, Cache<Uncached, Uncached, Uncached, Key>>,
    /// Checksums of what is in flash, so a save writes only what changed. Not copies: the
    /// config with week programs is tens of kilobytes the heap cannot spare.
    written: BTreeMap<Key, u32>,
    /// The small entries read back at runtime (trial boots, crashes).
    small: BTreeMap<Key, Vec<u8>>,
}

/// Room for one entry while the map reads or writes it. Taken for each operation rather
/// than kept: flash is touched a few times a day.
fn scratch() -> Vec<u8> {
    alloc::vec![0; BUFFER]
}

fn checksum(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

/// Entries kept in RAM as well: tiny, and read after boot.
fn kept(key: &Key) -> bool {
    *key == TRIAL_BOOTS || *key == CRASHES
}

/// `None` without a config partition: nothing is saved.
pub type SharedStore = Mutex<CriticalSectionRawMutex, Option<Store>>;

/// What was found in flash at boot.
pub struct Loaded {
    pub settings: Option<Settings>,
    pub config: Option<Config>,
}

impl Store {
    /// Open the `config` partition. `None` if the partition table has no such partition.
    pub fn open(mut flash: FlashStorage<'static>) -> Option<Store> {
        let mut table = [0u8; PARTITION_TABLE_MAX_LEN];
        let range = partitions::read_partition_table(&mut flash, &mut table).ok().and_then(|t| {
            t.iter()
                .find(|p| p.label_as_str() == CONFIG_LABEL && p.raw_type() == 0x01 && p.raw_subtype() == CONFIG_SUBTYPE)
                .map(|p| p.offset()..p.offset() + p.len())
        });
        let Some(range) = range else {
            log::error!("No config partition. Flash with --partition-table partitions.csv");
            return None;
        };
        let config = MapConfig::try_new(range).ok()?;
        Some(Store {
            map: MapStorage::new(Flash(flash), config, Cache::new_uncached()),
            written: BTreeMap::new(),
            small: BTreeMap::new(),
        })
    }

    async fn read_all(&mut self) -> Result<BTreeMap<Key, Vec<u8>>, Error> {
        let mut items = BTreeMap::new();
        let mut buf = scratch();
        let mut iter = self.map.fetch_all_items(&mut buf).await?;
        while let Some((key, value)) = iter.next::<Vec<u8>>(&mut buf).await? {
            // Later entries for the same key are newer.
            items.insert(key, value);
        }
        Ok(items)
    }

    /// Read everything. Unreadable flash (another firmware's data, or the two-slot format
    /// of earlier builds) is erased.
    pub async fn load(&mut self) -> Loaded {
        let items = match self.read_all().await {
            Ok(items) => items,
            Err(e) => {
                log::warn!("Stored data unreadable ({e:?}), erasing it");
                if let Err(e) = self.map.erase_all().await {
                    log::error!("Erasing failed: {e:?}");
                }
                BTreeMap::new()
            }
        };
        let text = |v: &Vec<u8>| core::str::from_utf8(v).ok().map(String::from);
        let settings = items.get(&SETTINGS).and_then(text).and_then(|j| match Settings::from_json(&j) {
            Ok(s) => Some(s),
            Err(e) => {
                log::error!("Stored settings are invalid ({e}), using defaults");
                None
            }
        });
        let config = items.get(&BRIDGE).and_then(text).and_then(|j| match Config::from_json(&j) {
            Ok(c) => Some(c),
            Err(e) => {
                log::error!("Stored bridge config is invalid ({e}), using defaults");
                None
            }
        });
        let config = config.map(|mut c| {
            for (key, value) in items.range([DEVICE, 0, 0, 0]..=[DEVICE, 0xFF, 0xFF, 0xFF]) {
                match serde_json::from_slice::<DeviceConfig>(value) {
                    Ok(d) => c.devices.push(d),
                    Err(e) => log::error!("Stored device {:02X?} is invalid: {e}", &key[1..]),
                }
            }
            c
        });
        self.written = items.iter().map(|(k, v)| (*k, checksum(v))).collect();
        self.small = items.into_iter().filter(|(k, _)| kept(k)).collect();
        Loaded { settings, config }
    }

    async fn put(&mut self, key: Key, value: Vec<u8>) -> Result<bool, Error> {
        let sum = checksum(&value);
        if self.written.get(&key) == Some(&sum) {
            return Ok(false);
        }
        if value.len() > BUFFER - 64 {
            return Err(Error::BufferTooSmall(value.len()));
        }
        self.map.store_item(&mut scratch(), &key, &value).await?;
        self.written.insert(key, sum);
        if kept(&key) {
            self.small.insert(key, value);
        }
        Ok(true)
    }

    /// Forgotten here only once it is gone from flash: after a failed delete, the next
    /// save tries again instead of the item coming back at the next boot.
    async fn remove(&mut self, key: Key) -> Result<(), Error> {
        if self.written.contains_key(&key) {
            self.map.remove_item(&mut scratch(), &key).await?;
            self.written.remove(&key);
        }
        self.small.remove(&key);
        Ok(())
    }

    pub async fn save_settings(&mut self, s: &Settings) -> Result<(), Error> {
        self.put(SETTINGS, s.to_json().into_bytes()).await?;
        log::info!("Saved settings");
        Ok(())
    }

    /// Write the parts of the bridge config that changed.
    pub async fn save_config(&mut self, mut config: Config) -> Result<(), Error> {
        let devices = core::mem::take(&mut config.devices);
        let mut changed = 0;
        let json = serde_json::to_vec(&config).unwrap_or_default();
        changed += self.put(BRIDGE, json).await? as usize;
        let mut keep = alloc::collections::BTreeSet::new();
        for d in &devices {
            let key = device_key(d.address);
            keep.insert(key);
            changed += self.put(key, serde_json::to_vec(d).unwrap_or_default()).await? as usize;
        }
        let gone: Vec<Key> = self.written.keys().filter(|k| k[0] == DEVICE && !keep.contains(*k)).copied().collect();
        for key in gone {
            self.remove(key).await?;
            changed += 1;
        }
        if changed > 0 {
            log::info!("Saved config ({changed} of {} entries changed)", devices.len() + 1);
        }
        Ok(())
    }

    /// The raw flash, for firmware updates (the app slots, not the map's range).
    pub fn flash(&mut self) -> &mut FlashStorage<'static> {
        &mut self.map.flash().0
    }

    pub fn crashes(&self) -> Vec<crate::crash::Crash> {
        self.small.get(&CRASHES).and_then(|v| serde_json::from_slice(v).ok()).unwrap_or_default()
    }

    /// Keep a crash, dropping the oldest beyond [`crate::crash::KEEP`].
    pub async fn add_crash(&mut self, c: crate::crash::Crash) {
        let mut all = self.crashes();
        all.push(c);
        let excess = all.len().saturating_sub(crate::crash::KEEP);
        all.drain(..excess);
        if let Err(e) = self.put(CRASHES, serde_json::to_vec(&all).unwrap_or_default()).await {
            log::error!("Saving the crash record failed: {e:?}");
        }
    }

    pub async fn clear_crashes(&mut self) -> Result<(), Error> {
        self.remove(CRASHES).await
    }

    pub async fn trial_boots(&mut self) -> u8 {
        self.small.get(&TRIAL_BOOTS).and_then(|v| v.first().copied()).unwrap_or(0)
    }

    pub async fn set_trial_boots(&mut self, n: u8) {
        let result = if n == 0 {
            self.remove(TRIAL_BOOTS).await
        } else {
            self.put(TRIAL_BOOTS, alloc::vec![n]).await.map(|_| ())
        };
        if let Err(e) = result {
            log::error!("Saving the firmware trial counter failed: {e:?}");
        }
    }

    /// Factory reset: settings and config.
    pub async fn erase(&mut self) -> Result<(), Error> {
        self.map.erase_all().await?;
        self.written.clear();
        self.small.clear();
        Ok(())
    }
}
