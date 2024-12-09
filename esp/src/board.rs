//! Supported boards: which GPIO sits at each D1 mini header position the shields use.
//!
//! Select one with a cargo feature (see `Cargo.toml` and `.cargo/config.toml` aliases).
//!
//! | Shield       | Header | `c3-mini` (Lolin C3 Mini) | `d1-mini-esp32` (MH-ET Live MiniKit / HW-665) |
//! |--------------|--------|---------------------------|-----------------------------------------------|
//! | CC1101 SCK   | D5     | GPIO2                     | GPIO18                                        |
//! | CC1101 MISO  | D6     | GPIO3                     | GPIO19                                        |
//! | CC1101 MOSI  | D7     | GPIO4                     | GPIO23                                        |
//! | CC1101 CSN   | D8     | GPIO5                     | GPIO5                                         |
//! | CC1101 GDO2  | D2     | GPIO8                     | GPIO21                                        |
//! | Burner relay | D1     | GPIO10                    | GPIO22                                        |

use esp_hal::gpio::AnyPin;

#[cfg(all(feature = "c3-mini", feature = "d1-mini-esp32"))]
compile_error!("Select exactly one board feature: `c3-mini` or `d1-mini-esp32`.");
#[cfg(not(any(feature = "c3-mini", feature = "d1-mini-esp32")))]
compile_error!("Select a board feature: `c3-mini` or `d1-mini-esp32`.");

pub struct Pins<'d> {
    pub sck: AnyPin<'d>,
    pub miso: AnyPin<'d>,
    pub mosi: AnyPin<'d>,
    pub cs: AnyPin<'d>,
    pub gdo2: AnyPin<'d>,
    pub relay: AnyPin<'d>,
}

#[cfg(feature = "c3-mini")]
pub const NAME: &str = "Lolin C3 Mini (ESP32-C3)";
#[cfg(feature = "d1-mini-esp32")]
pub const NAME: &str = "D1 mini ESP32 (ESP32-WROOM-32)";

/// Move this board's pins out of `esp_hal::peripherals::Peripherals`, leaving the rest usable.
#[cfg(feature = "c3-mini")]
macro_rules! take_pins {
    ($p:ident) => {
        $crate::board::Pins {
            sck: $p.GPIO2.into(),
            miso: $p.GPIO3.into(),
            mosi: $p.GPIO4.into(),
            cs: $p.GPIO5.into(),
            gdo2: $p.GPIO8.into(),
            relay: $p.GPIO10.into(),
        }
    };
}

#[cfg(feature = "d1-mini-esp32")]
macro_rules! take_pins {
    ($p:ident) => {
        $crate::board::Pins {
            sck: $p.GPIO18.into(),
            miso: $p.GPIO19.into(),
            mosi: $p.GPIO23.into(),
            cs: $p.GPIO5.into(),
            gdo2: $p.GPIO21.into(),
            relay: $p.GPIO22.into(),
        }
    };
}

pub(crate) use take_pins;
