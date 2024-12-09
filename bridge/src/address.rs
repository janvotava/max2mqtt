use alloc::string::String;
use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A 3-byte MAX! radio address, written as 6 hex digits (`"15A52A"`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Address(pub [u8; 3]);

impl Address {
    /// `000000`: used as destination for broadcasts and by unpaired devices.
    pub const BROADCAST: Address = Address([0, 0, 0]);

    pub fn is_broadcast(self) -> bool {
        self == Self::BROADCAST
    }

    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        match bytes {
            [a, b, c, ..] => Some(Address([*a, *b, *c])),
            _ => None,
        }
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02X}{:02X}{:02X}", self.0[0], self.0[1], self.0[2])
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseAddressError;

impl fmt::Display for ParseAddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("address must be exactly 6 hex digits")
    }
}

impl FromStr for Address {
    type Err = ParseAddressError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ParseAddressError);
        }
        let byte = |i: usize| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| ParseAddressError);
        Ok(Address([byte(0)?, byte(1)?, byte(2)?]))
    }
}

impl Serialize for Address {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Address {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Lower-case-insensitive hex dump, used for logging frames.
pub fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02X}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn parses_and_formats() {
        let a: Address = "15a52A".parse().unwrap();
        assert_eq!(a, Address([0x15, 0xA5, 0x2A]));
        assert_eq!(a.to_string(), "15A52A");
    }

    #[test]
    fn rejects_bad_input() {
        for s in ["", "12345", "1234567", "12345G", "+12345", "-1234"] {
            assert!(s.parse::<Address>().is_err(), "{s}");
        }
    }

    #[test]
    fn serde_roundtrip() {
        let a = Address([1, 2, 0xAB]);
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(json, "\"0102AB\"");
        assert_eq!(serde_json::from_str::<Address>(&json).unwrap(), a);
    }
}
