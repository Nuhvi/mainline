//! Helper functions and structs for signed peer announcements.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::{convert::TryFrom, time::SystemTime};

use crate::Id;

// TODO: update docs after getting a bep number, if ever.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// [BEP_xxxx](https://www.bittorrent.org/beps/bep_xxxx.html)'s signed Peer announce.
pub struct SignedAnnounce {
    /// ed25519 public key
    key: [u8; 32],
    /// timestamp of the signed announcement
    timestamp: u64,
    /// ed25519 signature
    #[serde(with = "serde_bytes")]
    signature: [u8; 64],
}

impl SignedAnnounce {
    /// Create a new mutable item from a signing key, value, sequence number and optional salt.
    pub fn new(signer: SigningKey, target: Id) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("time drift")
            .as_micros() as u64;

        let signable = encode_signable(target, timestamp);
        let signature = signer.sign(&signable);

        Self::new_signed_unchecked(
            signer.verifying_key().to_bytes(),
            timestamp,
            signature.into(),
        )
    }

    /// Create a new mutable item from an already signed value.
    pub fn new_signed_unchecked(key: [u8; 32], timestamp: u64, signature: [u8; 64]) -> Self {
        Self {
            key,
            timestamp,
            signature,
        }
    }

    pub(crate) fn from_dht_message(
        key: &[u8],
        target: Id,
        timestamp: u64,
        signature: &[u8],
    ) -> Result<Self, SignedAnnounceError> {
        let key = VerifyingKey::try_from(key)
            .map_err(|_| SignedAnnounceError::InvalidSignedAnnouncePublicKey)?;

        let signature = Signature::from_slice(signature)
            .map_err(|_| SignedAnnounceError::InvalidSignedAnnounceSignature)?;

        key.verify(&encode_signable(target, timestamp), &signature)
            .map_err(|_| SignedAnnounceError::InvalidSignedAnnounceSignature)?;

        Ok(Self {
            key: key.to_bytes(),
            timestamp,
            signature: signature.to_bytes(),
        })
    }

    // === Getters ===

    /// Returns a reference to the 32 bytes Ed25519 public key of this item.
    pub fn key(&self) -> &[u8; 32] {
        &self.key
    }

    /// Returns the `timestamp` of this announcement.
    pub fn timestamp(&self) -> u64 {
        self.timestamp
    }

    /// Returns the signature over this item.
    pub fn signature(&self) -> &[u8; 64] {
        &self.signature
    }
}

pub fn encode_signable(target: Id, timestamp: u64) -> Box<[u8]> {
    let mut signable = vec![];

    signable.extend(target.as_bytes());
    signable.extend(timestamp.to_be_bytes());

    signable.into()
}

#[derive(thiserror::Error, Debug)]
/// Mainline crate error enum.
pub enum SignedAnnounceError {
    #[error("Invalid mutable item signature")]
    /// Invalid mutable item signature
    InvalidSignedAnnounceSignature,

    #[error("Invalid mutable item public key")]
    /// Invalid mutable item public key
    InvalidSignedAnnouncePublicKey,
}
