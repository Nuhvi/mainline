//! Modules needed only for nodes running in server mode (not read-only).

pub mod peers;
pub mod request_filter;
pub mod request_handler;
pub mod tokens;

use std::{fmt::Debug, net::SocketAddrV4, num::NonZeroUsize};

use lru::LruCache;
use tracing::debug;

pub use request_filter::RequestFilter;

use crate::{
    common::{
        validate_immutable, AnnouncePeerRequestArguments, ErrorSpecific, FindNodeRequestArguments,
        FindNodeResponseArguments, GetImmutableResponseArguments, GetMutableResponseArguments,
        GetPeersRequestArguments, GetPeersResponseArguments, GetValueRequestArguments, Id,
        MutableItem, NoMoreRecentValueResponseArguments, NoValuesResponseArguments,
        PingResponseArguments, PutImmutableRequestArguments, PutMutableRequestArguments,
        PutRequest, PutRequestSpecific, RequestTypeSpecific, ResponseSpecific, RoutingTable,
    },
    rpc::server::{request_filter::DefaultFilter, request_handler::DefaultHandler},
};

use peers::PeersStore;
use tokens::Tokens;

pub use crate::common::RequestSpecific;
pub use request_handler::RequestHandler;

/// Default maximum number of info_hashes for which to store peers.
pub const MAX_INFO_HASHES: usize = 2000;
/// Default maximum number of peers to store per info_hash.
pub const MAX_PEERS: usize = 500;
/// Default maximum number of Immutable and Mutable items to store.
pub const MAX_VALUES: usize = 1000;

#[derive(Debug)]
/// A server that handles incoming requests.
///
/// Supports [BEP_005](https://www.bittorrent.org/beps/bep_0005.html) and [BEP_0044](https://www.bittorrent.org/beps/bep_0044.html).
///
/// But it doesn't implement any rate-limiting or blocking.
pub(crate) struct Server {
    /// Tokens generator
    tokens: Tokens,
    /// Peers store
    peers: PeersStore,
    /// Immutable values store
    immutable_values: LruCache<Id, Box<[u8]>>,
    /// Mutable values store
    mutable_values: LruCache<Id, MutableItem>,
    /// Filter requests before handling them.
    filter: Box<dyn RequestFilter>,
    /// Custom request handlers to allow adding new RPC methods
    /// other than the ones defined in supported BEPs.
    pub custom_request_handlers: Box<dyn RequestHandler>,
}

impl Default for Server {
    fn default() -> Self {
        Self::new(ServerSettings::default())
    }
}

#[derive(Debug, Clone)]
/// Settings for the default dht server.
pub struct ServerSettings {
    /// The maximum info_hashes for which to store peers.
    ///
    /// Defaults to [MAX_INFO_HASHES]
    pub max_info_hashes: usize,
    /// The maximum peers to store per info_hash.
    ///
    /// Defaults to [MAX_PEERS]
    pub max_peers_per_info_hash: usize,
    /// Maximum number of immutable values to store.
    ///
    /// Defaults to [MAX_VALUES]
    pub max_immutable_values: usize,
    /// Maximum number of mutable values to store.
    ///
    /// Defaults to [MAX_VALUES]
    pub max_mutable_values: usize,
    /// Filter requests before handling them.
    ///
    /// Defaults to a function that always returns true.
    pub filter: Box<dyn RequestFilter>,
    /// Custom request handlers to allow adding new RPC methods
    /// other than the ones defined in supported BEPs.
    pub custom_requests_handler: Box<dyn RequestHandler>,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            max_info_hashes: MAX_INFO_HASHES,
            max_peers_per_info_hash: MAX_PEERS,
            max_mutable_values: MAX_VALUES,
            max_immutable_values: MAX_VALUES,

            filter: Box::new(DefaultFilter),
            custom_requests_handler: Box::new(DefaultHandler),
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
/// Server Response types
pub enum ServerResponse {
    /// Normal query Response
    Response(ResponseSpecific),
    /// Error response
    Error(ErrorSpecific),
}

impl Server {
    /// Creates a new [Server]
    pub fn new(settings: ServerSettings) -> Self {
        let tokens = Tokens::new();

        Self {
            tokens,
            peers: PeersStore::new(
                NonZeroUsize::new(settings.max_info_hashes).unwrap_or(
                    NonZeroUsize::new(MAX_INFO_HASHES).expect("MAX_PEERS is NonZeroUsize"),
                ),
                NonZeroUsize::new(settings.max_peers_per_info_hash)
                    .unwrap_or(NonZeroUsize::new(MAX_PEERS).expect("MAX_PEERS is NonZeroUsize")),
            ),

            immutable_values: LruCache::new(
                NonZeroUsize::new(settings.max_immutable_values)
                    .unwrap_or(NonZeroUsize::new(MAX_VALUES).expect("MAX_VALUES is NonZeroUsize")),
            ),
            mutable_values: LruCache::new(
                NonZeroUsize::new(settings.max_mutable_values)
                    .unwrap_or(NonZeroUsize::new(MAX_VALUES).expect("MAX_VALUES is NonZeroUsize")),
            ),
            filter: settings.filter,
            custom_request_handlers: settings.custom_requests_handler,
        }
    }

    /// Returns an optional response or an error for a request.
    ///
    /// Passed to the Rpc to send back to the requester.
    pub fn handle_request(
        &mut self,
        routing_table: &RoutingTable,
        from: SocketAddrV4,
        request: RequestSpecific,
    ) -> Option<ServerResponse> {
        if !self.filter.allow_request(&request, from) {
            return None;
        }

        // Lazily rotate secrets before handling a request
        if self.tokens.should_update() {
            self.tokens.rotate()
        }

        let requester_id = request.requester_id;

        Some(match request.request_type {
            RequestTypeSpecific::Ping => {
                ServerResponse::Response(ResponseSpecific::Ping(PingResponseArguments {
                    responder_id: *routing_table.id(),
                }))
            }
            RequestTypeSpecific::FindNode(FindNodeRequestArguments { target, .. }) => {
                ServerResponse::Response(ResponseSpecific::FindNode(FindNodeResponseArguments {
                    responder_id: *routing_table.id(),
                    nodes: routing_table.closest(target),
                }))
            }
            RequestTypeSpecific::GetPeers(GetPeersRequestArguments { info_hash, .. }) => {
                ServerResponse::Response(match self.peers.get_random_peers(&info_hash) {
                    Some(peers) => ResponseSpecific::GetPeers(GetPeersResponseArguments {
                        responder_id: *routing_table.id(),
                        token: self.tokens.generate_token(from).into(),
                        nodes: Some(routing_table.closest(info_hash)),
                        values: peers,
                    }),
                    None => ResponseSpecific::NoValues(NoValuesResponseArguments {
                        responder_id: *routing_table.id(),
                        token: self.tokens.generate_token(from).into(),
                        nodes: Some(routing_table.closest(info_hash)),
                    }),
                })
            }
            RequestTypeSpecific::GetValue(GetValueRequestArguments { target, seq, .. }) => {
                if seq.is_some() {
                    ServerResponse::Response(self.handle_get_mutable(
                        routing_table,
                        from,
                        target,
                        seq,
                    ))
                } else if let Some(v) = self.immutable_values.get(&target) {
                    ServerResponse::Response(ResponseSpecific::GetImmutable(
                        GetImmutableResponseArguments {
                            responder_id: *routing_table.id(),
                            token: self.tokens.generate_token(from).into(),
                            nodes: Some(routing_table.closest(target)),
                            v: v.clone(),
                        },
                    ))
                } else {
                    ServerResponse::Response(self.handle_get_mutable(
                        routing_table,
                        from,
                        target,
                        seq,
                    ))
                }
            }
            RequestTypeSpecific::Put(PutRequest {
                token,
                put_request_type,
            }) => match put_request_type {
                PutRequestSpecific::AnnouncePeer(AnnouncePeerRequestArguments {
                    info_hash,
                    port,
                    implied_port,
                    ..
                }) => {
                    if !self.tokens.validate(from, &token) {
                        debug!(
                            ?info_hash,
                            ?requester_id,
                            ?from,
                            request_type = "announce_peer",
                            "Invalid token"
                        );

                        return Some(ServerResponse::Error(ErrorSpecific {
                            code: 203,
                            description: "Bad token".to_string(),
                        }));
                    }

                    let peer = match implied_port {
                        Some(true) => from,
                        _ => SocketAddrV4::new(*from.ip(), port),
                    };

                    self.peers
                        .add_peer(info_hash, (&request.requester_id, peer));

                    return Some(ServerResponse::Response(ResponseSpecific::Ping(
                        PingResponseArguments {
                            responder_id: *routing_table.id(),
                        },
                    )));
                }
                PutRequestSpecific::PutImmutable(PutImmutableRequestArguments {
                    v,
                    target,
                    ..
                }) => {
                    if !self.tokens.validate(from, &token) {
                        debug!(
                            ?target,
                            ?requester_id,
                            ?from,
                            request_type = "put_immutable",
                            "Invalid token"
                        );

                        return Some(ServerResponse::Error(ErrorSpecific {
                            code: 203,
                            description: "Bad token".to_string(),
                        }));
                    }

                    if v.len() > 1000 {
                        debug!(?target, ?requester_id, ?from, size = ?v.len(), "Message (v field) too big.");

                        return Some(ServerResponse::Error(ErrorSpecific {
                            code: 205,
                            description: "Message (v field) too big.".to_string(),
                        }));
                    }
                    if !validate_immutable(&v, target) {
                        debug!(?target, ?requester_id, ?from, v = ?v, "Target doesn't match the sha1 hash of v field.");

                        return Some(ServerResponse::Error(ErrorSpecific {
                            code: 203,
                            description: "Target doesn't match the sha1 hash of v field"
                                .to_string(),
                        }));
                    }

                    self.immutable_values.put(target, v);

                    return Some(ServerResponse::Response(ResponseSpecific::Ping(
                        PingResponseArguments {
                            responder_id: *routing_table.id(),
                        },
                    )));
                }
                PutRequestSpecific::PutMutable(PutMutableRequestArguments {
                    target,
                    v,
                    k,
                    seq,
                    sig,
                    salt,
                    cas,
                    ..
                }) => {
                    if !self.tokens.validate(from, &token) {
                        debug!(
                            ?target,
                            ?requester_id,
                            ?from,
                            request_type = "put_mutable",
                            "Invalid token"
                        );
                        return Some(ServerResponse::Error(ErrorSpecific {
                            code: 203,
                            description: "Bad token".to_string(),
                        }));
                    }
                    if v.len() > 1000 {
                        return Some(ServerResponse::Error(ErrorSpecific {
                            code: 205,
                            description: "Message (v field) too big.".to_string(),
                        }));
                    }
                    if let Some(ref salt) = salt {
                        if salt.len() > 64 {
                            return Some(ServerResponse::Error(ErrorSpecific {
                                code: 207,
                                description: "salt (salt field) too big.".to_string(),
                            }));
                        }
                    }
                    if let Some(previous) = self.mutable_values.get(&target) {
                        if let Some(cas) = cas {
                            if previous.seq() != cas {
                                debug!(
                                    ?target,
                                    ?requester_id,
                                    ?from,
                                    "CAS mismatched, re-read value and try again."
                                );

                                return Some(ServerResponse::Error(ErrorSpecific {
                                    code: 301,
                                    description: "CAS mismatched, re-read value and try again."
                                        .to_string(),
                                }));
                            }
                        };

                        if seq < previous.seq() {
                            debug!(
                                ?target,
                                ?requester_id,
                                ?from,
                                "Sequence number less than current."
                            );

                            return Some(ServerResponse::Error(ErrorSpecific {
                                code: 302,
                                description: "Sequence number less than current.".to_string(),
                            }));
                        }
                    }

                    match MutableItem::from_dht_message(target, &k, v, seq, &sig, salt) {
                        Ok(item) => {
                            self.mutable_values.put(target, item);

                            ServerResponse::Response(ResponseSpecific::Ping(
                                PingResponseArguments {
                                    responder_id: *routing_table.id(),
                                },
                            ))
                        }
                        Err(error) => {
                            debug!(?target, ?requester_id, ?from, ?error, "Invalid signature");

                            ServerResponse::Error(ErrorSpecific {
                                code: 206,
                                description: "Invalid signature".to_string(),
                            })
                        }
                    }
                }
            },
            RequestTypeSpecific::Unknown { q, arguments } => {
                if let Some(response) = self
                    .custom_request_handlers
                    .handle_request(q, arguments, from)
                {
                    response
                } else {
                    // Return a find_node response to help finding closest nodes.
                    ServerResponse::Response(ResponseSpecific::Ping(PingResponseArguments {
                        responder_id: *routing_table.id(),
                    }))
                }
            }
        })
    }

    /// Handle get mutable request
    fn handle_get_mutable(
        &mut self,
        routing_table: &RoutingTable,
        from: SocketAddrV4,
        target: Id,
        seq: Option<i64>,
    ) -> ResponseSpecific {
        match self.mutable_values.get(&target) {
            Some(item) => {
                let no_more_recent_values = seq.map(|request_seq| item.seq() <= request_seq);

                match no_more_recent_values {
                    Some(true) => {
                        ResponseSpecific::NoMoreRecentValue(NoMoreRecentValueResponseArguments {
                            responder_id: *routing_table.id(),
                            token: self.tokens.generate_token(from).into(),
                            nodes: Some(routing_table.closest(target)),
                            seq: item.seq(),
                        })
                    }
                    _ => ResponseSpecific::GetMutable(GetMutableResponseArguments {
                        responder_id: *routing_table.id(),
                        token: self.tokens.generate_token(from).into(),
                        nodes: Some(routing_table.closest(target)),
                        v: item.value().into(),
                        k: *item.key(),
                        seq: item.seq(),
                        sig: *item.signature(),
                    }),
                }
            }
            None => ResponseSpecific::NoValues(NoValuesResponseArguments {
                responder_id: *routing_table.id(),
                token: self.tokens.generate_token(from).into(),
                nodes: Some(routing_table.closest(target)),
            }),
        }
    }
}
