//! Stable 128-bit random ids, written as 22 base62 characters (`0-9A-Za-z`).
//! Used for nodes, variables, collections and modes.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
/// 62^22 > 2^128, so every u128 fits in 22 digits.
pub const ID_LEN: usize = 22;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id(pub u128);

/// Node ids, variable ids, collection ids and mode ids share one representation.
pub type NodeId = Id;

impl Id {
    pub fn to_base62(self) -> [u8; ID_LEN] {
        // Two u128 divisions split the value into 2 + 10 + 10 digits; the digits then come from
        // u64 arithmetic, which is much cheaper than 22 u128 divisions.
        const CHUNK: u128 = 62u128.pow(10);
        let mut out = [b'0'; ID_LEN];
        let (rest, low) = (self.0 / CHUNK, (self.0 % CHUNK) as u64);
        let (high, mid) = ((rest / CHUNK) as u64, (rest % CHUNK) as u64);
        for (range, mut v) in [(12..ID_LEN, low), (2..12, mid), (0..2, high)] {
            for slot in out[range].iter_mut().rev() {
                *slot = ALPHABET[(v % 62) as usize];
                v /= 62;
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdParseError {
    #[error("id must be {ID_LEN} base62 characters, got {0}")]
    Length(usize),
    #[error("invalid base62 character {0:?}")]
    Char(char),
    #[error("id is larger than 128 bits")]
    Overflow,
}

impl FromStr for Id {
    type Err = IdParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != ID_LEN {
            return Err(IdParseError::Length(s.chars().count()));
        }
        let mut v: u128 = 0;
        for c in s.bytes() {
            let d = match c {
                b'0'..=b'9' => c - b'0',
                b'A'..=b'Z' => c - b'A' + 10,
                b'a'..=b'z' => c - b'a' + 36,
                _ => return Err(IdParseError::Char(c as char)),
            };
            v = v.checked_mul(62).and_then(|v| v.checked_add(d as u128)).ok_or(IdParseError::Overflow)?;
        }
        Ok(Id(v))
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.to_base62();
        // ALPHABET is ASCII, so this never fails.
        f.write_str(std::str::from_utf8(&b).unwrap_or_default())
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id({self})")
    }
}

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let b = self.to_base62();
        s.serialize_str(std::str::from_utf8(&b).unwrap_or_default())
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = Id;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a 22-character base62 id")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Id, E> {
                v.parse().map_err(E::custom)
            }
        }
        d.deserialize_str(V)
    }
}

impl JsonSchema for Id {
    fn schema_name() -> Cow<'static, str> {
        "Id".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[0-7][0-9A-Za-z]{21}$",
            "description": "128-bit random id in base62 (22 characters; values above 2^128-1, i.e. above 7n42DGM5Tflk9n8mt7Fhc7, are rejected)"
        })
    }
}

/// Source of new ids: OS randomness by default, or a deterministic sequence for tests and for
/// agents that ask for reproducible ids.
#[derive(Debug, Clone)]
#[derive(Default)]
pub enum IdGen {
    #[default]
    Random,
    Seeded(u64),
}


impl IdGen {
    pub fn seeded(seed: u64) -> Self {
        IdGen::Seeded(seed)
    }

    pub fn next_id(&mut self) -> Id {
        match self {
            IdGen::Random => {
                let mut b = [0u8; 16];
                if getrandom::fill(&mut b).is_err() {
                    // No OS entropy: fall back to time + counter so ids stay unique per process.
                    return fallback_id();
                }
                Id(u128::from_le_bytes(b))
            }
            IdGen::Seeded(state) => {
                let hi = splitmix64(state);
                let lo = splitmix64(state);
                Id(((hi as u128) << 64) | lo as u128)
            }
        }
    }
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn fallback_id() -> Id {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mut s = (nanos as u64) ^ COUNTER.fetch_add(1, Ordering::Relaxed).rotate_left(32);
    Id(((splitmix64(&mut s) as u128) << 64) | splitmix64(&mut s) as u128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base62_round_trip_extremes() {
        for v in [0u128, 1, 61, 62, u64::MAX as u128, u128::MAX] {
            let s = Id(v).to_string();
            assert_eq!(s.len(), ID_LEN);
            assert_eq!(s.parse::<Id>(), Ok(Id(v)));
        }
        assert_eq!(Id(0).to_string(), "0000000000000000000000");
    }

    #[test]
    fn base62_matches_digit_by_digit() {
        let slow = |mut v: u128| {
            let mut out = [b'0'; ID_LEN];
            for slot in out.iter_mut().rev() {
                *slot = ALPHABET[(v % 62) as usize];
                v /= 62;
            }
            out
        };
        assert_eq!(&Id(u128::MAX).to_base62(), b"7n42DGM5Tflk9n8mt7Fhc7");
        let mut g = IdGen::seeded(3);
        for v in (0..2000).map(|_| g.next_id().0).chain([62u128.pow(10) - 1, 62u128.pow(10), 62u128.pow(20), 62u128.pow(20) - 1]) {
            assert_eq!(Id(v).to_base62(), slow(v), "{v}");
        }
    }

    #[test]
    fn rejects_bad_ids() {
        assert_eq!("abc".parse::<Id>(), Err(IdParseError::Length(3)));
        assert_eq!("000000000000000000000-".parse::<Id>(), Err(IdParseError::Char('-')));
        assert_eq!("zzzzzzzzzzzzzzzzzzzzzz".parse::<Id>(), Err(IdParseError::Overflow));
    }

    #[test]
    fn seeded_is_deterministic() {
        let (mut a, mut b) = (IdGen::seeded(7), IdGen::seeded(7));
        for _ in 0..10 {
            assert_eq!(a.next_id(), b.next_id());
        }
        assert_ne!(IdGen::seeded(7).next_id(), IdGen::seeded(8).next_id());
    }

    #[test]
    fn random_ids_differ() {
        let mut g = IdGen::Random;
        assert_ne!(g.next_id(), g.next_id());
    }
}
