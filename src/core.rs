use std::collections::HashMap;
use std::net::SocketAddrV4;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use lru::LruCache;
use tracing::info;

use iterative_query::IterativeQuery;
use put_query::PutQuery;

use crate::common::{Id, MessageType, Node, RequestSpecific, RequestTypeSpecific, RoutingTable};

pub(crate) mod iterative_query;
pub(crate) mod put_query;
pub(crate) mod server;

use server::Server;
use server::ServerSettings;

pub use put_query::{ConcurrencyError, PutError, PutQueryError};

pub(crate) const REFRESH_TABLE_INTERVAL: Duration = Duration::from_secs(15 * 60);
pub(crate) const PING_TABLE_INTERVAL: Duration = Duration::from_secs(5 * 60);

pub(crate) const MAX_CACHED_ITERATIVE_QUERIES: usize = 1000;

pub(crate) const VERSION: [u8; 4] = [82, 83, 0, 6]; // "RS" version 06

pub(crate) const VERSIONS_SUPPORTING_SIGNED_PEERS: &[[u8; 4]] = &[
    // This node
    VERSION,
    // Add more nodes as we learn about supporting clients
    // b"LT.."
];

#[derive(Debug)]
/// Side effect free Core
pub struct Core {
    // Options
    pub(crate) bootstrap: Box<[SocketAddrV4]>,

    // Routing
    /// Closest nodes to this node
    pub(crate) routing_table: RoutingTable,
    /// Closest nodes to this node that support the signed peers
    /// [BEP_????](https://github.com/Nuhvi/mainline/blob/main/beps/bep_signed_peers.rst) proposal.
    pub(crate) signed_peers_routing_table: RoutingTable,
    /// Last time we refreshed the routing table with a find_node query.
    pub(crate) last_table_refresh: Instant,
    /// Last time we pinged nodes in the routing table.
    pub(crate) last_table_ping: Instant,

    /// Closest responding nodes to specific target
    pub(crate) cached_iterative_queries: LruCache<Id, CachedIterativeQuery>,
    /// Active IterativeQueries
    pub(crate) iterative_queries: HashMap<Id, IterativeQuery>,
    /// Put queries are special, since they have to wait for a corresponding
    /// get query to finish, update the closest_nodes, then `query_all` these.
    pub(crate) put_queries: HashMap<Id, PutQuery>,

    pub(crate) server: Server,
    pub(crate) public_address: Option<SocketAddrV4>,
    pub(crate) firewalled: bool,
    pub(crate) server_mode: bool,
}

impl Core {
    pub fn new(
        id: Id,
        bootstrap: Vec<SocketAddrV4>,
        server_mode: bool,
        server_settings: ServerSettings,
    ) -> Self {
        Self {
            bootstrap: bootstrap.into(),
            routing_table: RoutingTable::new(id),
            signed_peers_routing_table: RoutingTable::new(id),
            iterative_queries: HashMap::new(),
            put_queries: HashMap::new(),
            cached_iterative_queries: LruCache::new(
                NonZeroUsize::new(MAX_CACHED_ITERATIVE_QUERIES)
                    .expect("MAX_CACHED_BUCKETS is NonZeroUsize"),
            ),
            last_table_refresh: Instant::now(),
            last_table_ping: Instant::now(),
            server: Server::new(server_settings),
            public_address: None,
            firewalled: true,
            server_mode,
        }
    }

    fn id(&self) -> &Id {
        self.routing_table.id()
    }

    pub fn handle_request(
        &mut self,
        from: SocketAddrV4,
        request_from_read_only_node: bool,
        version: Option<[u8; 4]>,
        request_specific: RequestSpecific,
    ) -> (Option<MessageType>, bool) {
        self.maybe_add_node_from_request(
            from,
            version,
            request_from_read_only_node,
            &request_specific,
        );

        let should_repopulate_routing_tables =
            self.does_verify_our_new_public_address_with_self_ping(from, &request_specific);

        let response = if self.server_mode {
            let server = &mut self.server;
            let response = server.handle_request(
                &self.routing_table,
                &self.signed_peers_routing_table,
                from,
                request_specific,
            );

            match response {
                Some(MessageType::Error(_)) | Some(MessageType::Response(_)) => response,
                _ => None,
            }
        } else {
            None
        };

        (response, should_repopulate_routing_tables)
    }

    /// In client mode, we never add to our table any node contacting us
    /// without querying it first, to avoid eclipse attacks.
    ///
    /// While bootstrapping though, we can't be that picky, we have two
    /// reasons to except that rule:
    ///
    /// 1. first node creating the DHT;
    ///    without this exception, the bootstrapping node's routing table will never be populated.
    /// 2. Bootstrapping signed_peers_routing_table requires that we latch to any node
    ///    that claims to support it.
    ///    In `periodic_node_maintaenance` fake unresponsive nodes will be removed.
    ///    And either way we prioritize secure nodes, so making up nodes from same
    //    machine won't have much effect.
    fn maybe_add_node_from_request(
        &mut self,
        from: SocketAddrV4,
        version: Option<[u8; 4]>,
        request_from_read_only_node: bool,
        request_specific: &RequestSpecific,
    ) {
        if self.server_mode && !request_from_read_only_node {
            if let RequestTypeSpecific::FindNode(ref param) = request_specific.request_type {
                let node = Node::new(param.target, from);
                let supports_signed_peers = supports_signed_peers(version);

                if self.bootstrap.is_empty() {
                    self.routing_table.add(node.clone());

                    if supports_signed_peers {
                        self.signed_peers_routing_table.add(node);
                    }
                } else if supports_signed_peers {
                    self.signed_peers_routing_table.add(node);
                }
            }
        }
    }

    fn does_verify_our_new_public_address_with_self_ping(
        &mut self,
        from: SocketAddrV4,
        request_specific: &RequestSpecific,
    ) -> bool {
        if let Some(our_address) = self.public_address {
            let is_ping = matches!(request_specific.request_type, RequestTypeSpecific::Ping);

            if from == our_address && is_ping {
                self.firewalled = false;

                let ipv4 = our_address.ip();

                // Restarting our routing table with new secure Id if necessary.
                if !self.id().is_valid_for_ip(*ipv4) {
                    let new_id = Id::from_ipv4(*ipv4);

                    info!(
                        "Our current id {} is not valid for adrsess {}. Using new id {}",
                        self.id(),
                        our_address,
                        new_id
                    );

                    self.routing_table.reset_id(new_id);
                    self.signed_peers_routing_table.reset_id(new_id);

                    return true;
                }
            }
        }

        false
    }
}

pub(crate) fn supports_signed_peers(version: Option<[u8; 4]>) -> bool {
    version
        .map(|version| {
            VERSIONS_SUPPORTING_SIGNED_PEERS
                .iter()
                .any(|v| version[0..2] == v[0..2] && version[2..] >= v[2..])
        })
        .unwrap_or_default()
}

pub(crate) struct CachedIterativeQuery {
    pub(crate) closest_responding_nodes: Box<[Node]>,
    pub(crate) dht_size_estimate: f64,
    pub(crate) responders_dht_size_estimate: f64,
    pub(crate) subnets: u8,

    pub(crate) request_type: RequestTypeSpecific,
}
