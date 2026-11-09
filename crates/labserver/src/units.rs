//! Human-friendly sizes (`"64KiB"`, `"1.5GB"`) and rates (`"2MB/s"`) for
//! scenario files. Both serialize back as plain byte counts.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// A byte count. In TOML: an integer, or a string with a unit — decimal
/// (`kB`/`KB`, `MB`, `GB`, `TB`) or binary (`KiB`, `MiB`, `GiB`, `TiB`).
/// Units are case-insensitive; `B` alone means bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Size(pub u64);

/// Bytes per second. Same syntax as [`Size`], with an optional `/s` suffix:
/// `"2MB/s"` is 2 000 000 B/s, `"2MiB/s"` is 2 097 152 B/s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Rate(pub u64);

/// Parses `"10MiB"`, `"1.5 GB"`, `"512"`, … into a byte count.
pub fn parse_size(text: &str) -> Result<u64, String> {
    let s = text.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '_'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let num: String = num.chars().filter(|&c| c != '_').collect();
    if num.is_empty() {
        return Err(format!("`{text}`: expected a number"));
    }
    let mult: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1_000,
        "m" | "mb" => 1_000_000,
        "g" | "gb" => 1_000_000_000,
        "t" | "tb" => 1_000_000_000_000,
        "ki" | "kib" => 1 << 10,
        "mi" | "mib" => 1 << 20,
        "gi" | "gib" => 1 << 30,
        "ti" | "tib" => 1 << 40,
        other => return Err(format!("`{text}`: unknown unit `{other}`")),
    };
    if num.contains('.') {
        let v: f64 = num.parse().map_err(|_| format!("`{text}`: bad number"))?;
        let bytes = v * mult as f64;
        if !(0.0..1.8e19).contains(&bytes) {
            return Err(format!("`{text}`: out of range"));
        }
        Ok(bytes.round() as u64)
    } else {
        let v: u64 = num.parse().map_err(|_| format!("`{text}`: bad number"))?;
        v.checked_mul(mult)
            .ok_or_else(|| format!("`{text}`: out of range"))
    }
}

/// Parses `"2MB/s"`, `"2 MiB/s"`, `"500000"` into bytes per second.
pub fn parse_rate(text: &str) -> Result<u64, String> {
    let s = text.trim();
    let s = s
        .strip_suffix("/s")
        .or_else(|| s.strip_suffix("ps"))
        .unwrap_or(s);
    parse_size(s)
}

#[derive(Deserialize)]
#[serde(untagged)]
enum NumOrStr {
    Num(u64),
    Str(String),
}

impl<'de> Deserialize<'de> for Size {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match NumOrStr::deserialize(d)? {
            NumOrStr::Num(n) => Ok(Size(n)),
            NumOrStr::Str(s) => parse_size(&s).map(Size).map_err(serde::de::Error::custom),
        }
    }
}

impl<'de> Deserialize<'de> for Rate {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match NumOrStr::deserialize(d)? {
            NumOrStr::Num(n) => Ok(Rate(n)),
            NumOrStr::Str(s) => parse_rate(&s).map(Rate).map_err(serde::de::Error::custom),
        }
    }
}

impl Serialize for Size {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(self.0)
    }
}

impl Serialize for Rate {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(self.0)
    }
}

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} B", self.0)
    }
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} B/s", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("0").unwrap(), 0);
        assert_eq!(parse_size("512").unwrap(), 512);
        assert_eq!(parse_size("64KiB").unwrap(), 65_536);
        assert_eq!(parse_size("1 GiB").unwrap(), 1 << 30);
        assert_eq!(parse_size("10MB").unwrap(), 10_000_000);
        assert_eq!(parse_size("1.5mib").unwrap(), 1_572_864);
        assert_eq!(parse_size("1_000").unwrap(), 1000);
        assert!(parse_size("ten").is_err());
        assert!(parse_size("5 parsecs").is_err());
        assert!(parse_size("99999999999TiB").is_err());
    }

    #[test]
    fn rates() {
        assert_eq!(parse_rate("2MB/s").unwrap(), 2_000_000);
        assert_eq!(parse_rate("2MiB/s").unwrap(), 2 << 20);
        assert_eq!(parse_rate("100kBps").unwrap(), 100_000);
        assert_eq!(parse_rate("4096").unwrap(), 4096);
    }

    #[test]
    fn serde_accepts_both_forms() {
        #[derive(Deserialize)]
        struct T {
            a: Size,
            b: Rate,
        }
        let t: T = toml::from_str("a = \"1KiB\"\nb = 300").unwrap();
        assert_eq!((t.a, t.b), (Size(1024), Rate(300)));
    }
}
