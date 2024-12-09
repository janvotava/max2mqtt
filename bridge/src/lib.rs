//! Everything in max2mqtt that is not hardware: the eQ-3 MAX! radio protocol, the heating
//! controller, the transmit queue, a minimal MQTT 3.1.1 codec, Home Assistant discovery, the
//! device settings, and the bridge that ties them together.
//!
//! The crate is `no_std` + `alloc` so the firmware can use it as is. It never touches
//! hardware or reads a clock: callers pass the monotonic time in milliseconds (and the local
//! wall clock, once known) into every call, which is what makes it testable on the host with
//! `cargo test`.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod address;
pub mod backup;
pub mod bridge;
pub mod clock;
pub mod config;
pub mod heating;
pub mod homeassistant;
pub mod mqtt;
pub mod protocol;
pub mod schedule;
pub mod settings;
pub mod tx;

pub use address::Address;
pub use bridge::{Bridge, Ctx, Effect, Topics};
pub use config::Config;
pub use settings::Settings;

/// Monotonic milliseconds since boot. Never wraps (u64), unlike Arduino's `millis()`.
pub type Millis = u64;
