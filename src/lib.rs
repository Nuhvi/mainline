#![doc = include_str!("../README.md")]
//! ## Feature flags
#![doc = document_features::document_features!()]
//!

/// Single threaded Actor model node
mod actor;
mod common;
/// Functional core testable separately from I/O
mod core;
mod dht;

pub use actor::config::Config;
pub use actor::Info;
pub use common::messages::{
    AnnouncePeerRequestArguments, AnnounceSignedPeerRequestArguments, ErrorSpecific,
    FindNodeRequestArguments, FindNodeResponseArguments, GetImmutableResponseArguments,
    GetMutableResponseArguments, GetPeersRequestArguments, GetPeersResponseArguments,
    GetSignedPeersResponseArguments, GetValueRequestArguments, MessageType,
    NoMoreRecentValueResponseArguments, NoValuesResponseArguments, PingResponseArguments,
    PutImmutableRequestArguments, PutMutableRequestArguments, PutRequest, PutRequestSpecific,
    RequestSpecific, RequestTypeSpecific, ResponseSpecific,
};
pub use common::{
    ClosestNodes, DhtSizeEstimate, Id, InvalidIdSize, MutableItem, Node, RoutingTable,
};
pub use core::server::{RequestFilter, ServerSettings, MAX_INFO_HASHES, MAX_PEERS, MAX_VALUES};
pub use dht::{Dht, DhtBuilder, Testnet};

#[cfg(feature = "async")]
pub use dht::async_dht;

pub use ed25519_dalek::SigningKey;

pub mod errors {
    //! Exported errors
    pub use super::common::{DecodeIdError, ErrorSpecific, MutableError};
    pub use super::core::{ConcurrencyError, PutError, PutQueryError};
    pub use super::dht::PutMutableError;
}
