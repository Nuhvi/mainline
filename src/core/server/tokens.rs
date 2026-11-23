//! Manage tokens for remote client IPs.

use sha1_smol::Sha1;
use std::fmt::{self, Debug, Formatter};
use std::net::SocketAddrV4;
use std::time::Instant;

use tracing::trace;

use crate::Id;

const SECRET_SIZE: usize = 20;
const TOKEN_SIZE: usize = 4;

/// Tokens generator.
///
/// A token binds the requester's IP, a rotating secret, and the request target.
/// The construction follows libtorrent:
/// `SHA1(ip string || secret || target)`, truncated to 4 bytes.
///
/// Read [BEP_0005](https://www.bittorrent.org/beps/bep_0005.html) for more information.
#[derive(Clone)]
pub(crate) struct Tokens {
    prev_secret: [u8; SECRET_SIZE],
    curr_secret: [u8; SECRET_SIZE],
    last_updated: Instant,
}

impl Debug for Tokens {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Tokens (_)")
    }
}

impl Tokens {
    /// Create a Tokens generator.
    pub(crate) fn new() -> Self {
        Tokens {
            prev_secret: random(),
            curr_secret: random(),
            last_updated: Instant::now(),
        }
    }

    // === Public Methods ===

    /// Returns `true` if the current secret needs to be updated after an interval.
    pub(crate) fn should_update(&self) -> bool {
        self.last_updated.elapsed() > crate::common::TOKEN_ROTATE_INTERVAL
    }

    /// Validate that the token was generated for `target` within the past 10 minutes.
    pub(crate) fn validate(&mut self, address: SocketAddrV4, token: &[u8], target: &Id) -> bool {
        if token.len() != TOKEN_SIZE {
            return false;
        }

        let prev = generate_token(address, self.prev_secret, target);
        let curr = generate_token(address, self.curr_secret, target);
        token == curr || token == prev
    }

    /// Rotate the tokens secret.
    pub(crate) fn rotate(&mut self) {
        trace!("Rotating secrets");

        self.prev_secret = self.curr_secret;
        self.curr_secret = random();

        self.last_updated = Instant::now();
    }

    /// Generates a new token for a remote peer and `target`.
    pub(crate) fn generate_token(
        &mut self,
        address: SocketAddrV4,
        target: &Id,
    ) -> [u8; TOKEN_SIZE] {
        generate_token(address, self.curr_secret, target)
    }
}

fn generate_token(
    address: SocketAddrV4,
    secret: [u8; SECRET_SIZE],
    target: &Id,
) -> [u8; TOKEN_SIZE] {
    let mut hasher = Sha1::new();
    let ip = address.ip().to_string();
    hasher.update(ip.as_bytes());
    hasher.update(&secret);
    hasher.update(target.as_bytes());

    let [a, b, c, d, ..] = hasher.digest().bytes();
    [a, b, c, d]
}

impl Default for Tokens {
    fn default() -> Self {
        Self::new()
    }
}

fn random() -> [u8; SECRET_SIZE] {
    let mut bytes = [0_u8; SECRET_SIZE];
    crate::common::fill_random(&mut bytes);

    bytes
}

#[cfg(test)]
mod tests {
    use eyre::Result;

    use super::*;

    #[test]
    fn valid_tokens() {
        let mut tokens = Tokens::new();

        let address = SocketAddrV4::new([127, 0, 0, 1].into(), 6881);
        let target = Id::random();
        let token = tokens.generate_token(address, &target);

        assert!(tokens.validate(address, &token, &target));
    }

    #[test]
    fn token_matches_sha1_construction() -> Result<()> {
        let mut tokens = Tokens {
            prev_secret: [0; SECRET_SIZE],
            curr_secret: [0; SECRET_SIZE],
            last_updated: Instant::now(),
        };
        let address = SocketAddrV4::new([127, 0, 0, 1].into(), 6881);
        let target = Id::from_bytes([0x11; 20])?;

        // SHA1("127.0.0.1" || 20 zero bytes || 0x11 * 20)[..4].
        assert_eq!(
            tokens.generate_token(address, &target),
            [0xcb, 0xef, 0x73, 0x03]
        );
        Ok(())
    }

    #[test]
    fn token_is_bound_to_target() {
        let mut tokens = Tokens::new();

        let address = SocketAddrV4::new([127, 0, 0, 1].into(), 6881);
        let target = Id::random();
        let other_target = Id::random();
        let token = tokens.generate_token(address, &target);

        assert!(tokens.validate(address, &token, &target));
        assert!(!tokens.validate(address, &token, &other_target));
    }

    #[test]
    fn token_is_bound_to_address() {
        let mut tokens = Tokens::new();

        let address = SocketAddrV4::new([127, 0, 0, 1].into(), 6881);
        let other_address = SocketAddrV4::new([127, 0, 0, 2].into(), 6881);
        let target = Id::random();
        let token = tokens.generate_token(address, &target);

        assert!(tokens.validate(address, &token, &target));
        assert!(!tokens.validate(other_address, &token, &target));
    }

    #[test]
    fn token_must_be_four_bytes() {
        let mut tokens = Tokens::new();

        let address = SocketAddrV4::new([127, 0, 0, 1].into(), 6881);
        let target = Id::random();

        assert!(!tokens.validate(address, &[], &target));
        assert!(!tokens.validate(address, &[0; 5], &target));
    }
}
