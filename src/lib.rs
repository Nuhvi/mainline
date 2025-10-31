#![doc = include_str!("../README.md")]
//! ## Feature flags
#![doc = document_features::document_features!()]
//!

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

/// Single threaded Actor model node
mod actor;
mod common;
/// Functional core testable separately from I/O
mod core;
mod dht;

pub use common::{
    messages::{MessageType, PutRequestSpecific, RequestSpecific},
    ClosestNodes, Id, MutableItem, Node, RoutingTable,
};
pub use core::server::{RequestFilter, ServerSettings, MAX_INFO_HASHES, MAX_PEERS, MAX_VALUES};
pub use dht::{Dht, DhtBuilder, Testnet};

pub use ed25519_dalek::SigningKey;

pub mod errors {
    //! Exported errors
    pub use super::common::ErrorSpecific;
    pub use super::core::{ConcurrencyError, PutError, PutQueryError};
    pub use super::dht::PutMutableError;

    pub use super::common::DecodeIdError;
    pub use super::common::MutableError;
}
