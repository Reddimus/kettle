use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

pub const PICKUP_LIFETIME: Duration = Duration::from_secs(5);
const MAX_PICKUPS: usize = 128;

/// An opaque launch-bound hook permit. Diagnostics never print its value.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PickupPermit([u8; 16]);

impl PickupPermit {
    pub fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
}

impl fmt::Debug for PickupPermit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PickupPermit([redacted])")
    }
}

impl serde::Serialize for PickupPermit {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut text = String::with_capacity(32);
        for byte in self.0 {
            text.push(char::from(HEX[usize::from(byte >> 4)]));
            text.push(char::from(HEX[usize::from(byte & 15)]));
        }
        serializer.serialize_str(&text)
    }
}

impl<'de> serde::Deserialize<'de> for PickupPermit {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = PickupPermit;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a pickup permit")
            }

            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Self::Value, E> {
                fn digit(byte: u8) -> Option<u8> {
                    match byte {
                        b'0'..=b'9' => Some(byte - b'0'),
                        b'a'..=b'f' => Some(byte - b'a' + 10),
                        _ => None,
                    }
                }
                if text.len() != 32 {
                    return Err(E::custom("invalid pickup permit"));
                }
                let mut bytes = [0; 16];
                let (pairs, _) = text.as_bytes().as_chunks::<2>();
                for (slot, pair) in bytes.iter_mut().zip(pairs) {
                    let high = digit(pair[0]).ok_or_else(|| E::custom("invalid pickup permit"))?;
                    let low = digit(pair[1]).ok_or_else(|| E::custom("invalid pickup permit"))?;
                    *slot = high * 16 + low;
                }
                Ok(PickupPermit(bytes))
            }
        }
        deserializer.deserialize_str(Visitor)
    }
}

/// Expiring one-use permits. Payloads stay inside the GUI; only the opaque
/// key travels to the display adapter, never to model content.
pub struct PickupBook<T> {
    slots: HashMap<PickupPermit, (T, Instant)>,
}

impl<T> Default for PickupBook<T> {
    fn default() -> Self {
        Self {
            slots: HashMap::new(),
        }
    }
}

impl<T> PickupBook<T> {
    pub fn issue(&mut self, key: PickupPermit, payload: T, now: Instant) -> bool {
        self.prune(now);
        if self.slots.len() >= MAX_PICKUPS || self.slots.contains_key(&key) {
            return false;
        }
        self.slots.insert(key, (payload, now + PICKUP_LIFETIME));
        true
    }

    pub fn prune(&mut self, now: Instant) {
        self.slots.retain(|_, (_, expires)| now < *expires);
    }

    pub fn take(&mut self, key: PickupPermit, now: Instant) -> Option<T> {
        let (payload, expires) = self.slots.remove(&key)?;
        (now < expires).then_some(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_round_trip_without_debug_or_error_disclosure() {
        let key = PickupPermit::new([171; 16]);
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(serde_json::from_str::<PickupPermit>(&json).unwrap(), key);
        assert_eq!(format!("{key:?}"), "PickupPermit([redacted])");
        for text in [
            "a".repeat(31),
            "a".repeat(33),
            "X".repeat(32),
            "A".repeat(32),
        ] {
            let error = serde_json::from_value::<PickupPermit>(serde_json::json!(text))
                .unwrap_err()
                .to_string();
            assert!(!error.contains(&text));
        }
    }

    #[test]
    fn pickup_is_one_use_and_live_collisions_cannot_replace_it() {
        let mut book = PickupBook::default();
        let now = Instant::now();
        let key = PickupPermit::new([1; 16]);
        assert!(book.issue(key, "first", now));
        assert!(!book.issue(key, "other", now));
        assert_eq!(book.take(key, now), Some("first"));
        assert_eq!(book.take(key, now), None);
    }

    #[test]
    fn pickup_storage_is_bounded_and_expiry_releases_capacity() {
        let mut book = PickupBook::default();
        let now = Instant::now();
        let key = |index: usize| {
            let mut bytes = [0; 16];
            bytes[..8].copy_from_slice(&(index as u64).to_le_bytes());
            PickupPermit::new(bytes)
        };
        for index in 0..MAX_PICKUPS {
            assert!(book.issue(key(index), index, now));
        }
        assert!(!book.issue(key(MAX_PICKUPS), MAX_PICKUPS, now));
        assert_eq!(book.take(key(0), now + PICKUP_LIFETIME), None);
        assert!(book.issue(key(MAX_PICKUPS), MAX_PICKUPS, now + PICKUP_LIFETIME));
        assert_eq!(book.take(key(1), now + PICKUP_LIFETIME), None);
        assert!(book.slots.len() <= 1);
    }
}
