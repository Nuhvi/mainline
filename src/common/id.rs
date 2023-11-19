//! Kademlia node Id or a lookup target
use rand::Rng;
use std::{
    fmt::{self, Debug, Display, Formatter},
    str::FromStr,
};

use crate::{Error, Result};

/// The size of node IDs in bits.
pub const ID_SIZE: usize = 20;
pub const MAX_DISTANCE: u8 = ID_SIZE as u8 * 8;

#[derive(Clone, Copy, PartialEq, Ord, PartialOrd, Eq, Hash)]
/// Kademlia node Id or a lookup target
pub struct Id {
    bytes: [u8; ID_SIZE],
}

impl Id {
    pub fn random() -> Id {
        let mut rng = rand::thread_rng();
        let bytes: [u8; 20] = rng.gen();

        Id { bytes }
    }
    /// Create a new Id from some bytes. Returns Err if `bytes` is not of length
    /// [ID_SIZE](crate::common::ID_SIZE).
    pub fn from_bytes<T: AsRef<[u8]>>(bytes: T) -> Result<Id> {
        let bytes = bytes.as_ref();
        if bytes.len() != ID_SIZE {
            return Err(Error::InvalidIdSize(bytes.len()));
        }

        let mut tmp: [u8; ID_SIZE] = [0; ID_SIZE];
        tmp[..ID_SIZE].clone_from_slice(&bytes[..ID_SIZE]);

        Ok(Id { bytes: tmp })
    }

    /// Simplified XOR distance between this Id and a target Id.
    ///
    /// The distance is the number of trailing non zero bits in the XOR result.
    ///
    /// Distance to self is 0
    /// Distance to the furthest Id is 160
    /// Distance to an Id with 5 leading matching bits is 155
    pub fn distance(&self, other: &Id) -> u8 {
        for (i, (a, b)) in self.bytes.iter().zip(other.bytes).enumerate() {
            if a != &b {
                // leading zeros so far + laedinge zeros of this byte
                let leading_zeros = (i as u32 * 8 + (a ^ b).leading_zeros()) as u8;

                return MAX_DISTANCE - leading_zeros;
            }
        }

        0
    }

    pub fn to_vec(self) -> Vec<u8> {
        self.bytes.to_vec()
    }
}

impl Display for Id {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let hex_chars: String = self
            .bytes
            .iter()
            .map(|byte| format!("{:02x}", byte))
            .collect();

        write!(f, "{}", hex_chars)
    }
}

impl From<[u8; ID_SIZE]> for Id {
    fn from(bytes: [u8; ID_SIZE]) -> Id {
        Id { bytes }
    }
}

impl FromStr for Id {
    type Err = Error;

    fn from_str(s: &str) -> Result<Id> {
        if s.len() % 2 != 0 {
            return Err(Error::InvalidIdEncoding(
                "Number of Hex characters should be even".into(),
            ));
        }

        let mut bytes = Vec::with_capacity(s.len() / 2);

        for i in 0..s.len() / 2 {
            let byte_str = &s[i * 2..(i * 2) + 2];
            if let Ok(byte) = u8::from_str_radix(byte_str, 16) {
                bytes.push(byte);
            } else {
                return Err(Error::Static("Invalid hex character")); // Invalid hex character
            }
        }

        Id::from_bytes(bytes)
    }
}

impl Debug for Id {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Id({})", self)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn distance_to_self() {
        let id = Id::random();
        let distance = id.distance(&id);
        assert_eq!(distance, 0)
    }

    #[test]
    fn distance_to_id() {
        let id = Id::from_str("0639A1E24FBB8AB277DF033476AB0DE10FAB3BDC").unwrap();

        let target = Id::from_str("035b1aeb9737ade1a80933594f405d3f772aa08e").unwrap();

        let distance = id.distance(&target);

        assert_eq!(distance, 155)
    }

    #[test]
    fn distance_to_random_id() {
        let id = Id::random();
        let target = Id::random();

        let distance = id.distance(&target);

        assert_ne!(distance, 0)
    }

    #[test]
    fn distance_to_furthest() {
        let id = Id::random();

        let mut opposite = [0_u8; 20];
        for (i, &value) in id.bytes.iter().enumerate() {
            opposite[i] = value ^ 0xff;
        }
        let target = Id::from_bytes(opposite).unwrap();

        let distance = id.distance(&target);

        assert_eq!(distance, MAX_DISTANCE)
    }

    #[test]
    fn from_u8_20() {
        let bytes = [0_08; 20];

        let id: Id = bytes.into();

        assert_eq!(id.bytes, bytes);
    }
}
