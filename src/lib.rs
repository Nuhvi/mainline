#![doc = include_str!("../README.md")]
//! ## Feature flags
#![doc = document_features::document_features!()]
//!

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

mod common;
mod dht;
mod rpc;

// Public modules
#[cfg(feature = "async")]
pub mod async_dht;

pub use common::{Id, MutableItem, Node, RoutingTable};

pub use dht::{Dht, DhtBuilder, Testnet};
pub use rpc::{
    messages::{MessageType, PutRequestSpecific, RequestSpecific},
    server::{RequestFilter, ServerSettings, MAX_INFO_HASHES, MAX_PEERS, MAX_VALUES},
    ClosestNodes,
};

pub use ed25519_dalek::SigningKey;

pub mod errors {
    //! Exported errors
    pub use super::common::ErrorSpecific;
    pub use super::dht::PutMutableError;
    pub use super::rpc::{ConcurrencyError, PutError, PutQueryError};

    pub use super::common::DecodeIdError;
    pub use super::common::MutableError;
}
