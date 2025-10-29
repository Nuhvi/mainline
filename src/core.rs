use std::collections::HashMap;
use std::net::SocketAddrV4;
use std::num::NonZeroUsize;

use lru::LruCache;
use tracing::{debug, info, trace};

use iterative_query::IterativeQuery;
use put_query::PutQuery;

use crate::common::{Id, MessageType, Node, RequestSpecific, RequestTypeSpecific, RoutingTable};

pub(crate) mod iterative_query;
pub(crate) mod put_query;
pub(crate) mod server;

use server::Server;
use server::ServerSettings;

pub use put_query::{ConcurrencyError, PutError, PutQueryError};

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

    /// Check an incoming request, and if it is a PING request, with our own
    /// id and from our own address, then we assume that the ip address we are being
    /// told is valid.
    // TODO: can this be spoofed? should we check the `tid` as well to be sure?
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

    /// Remove done [IterativeQuery]s, return the [Id]s of [PutQuery] ready to start,
    /// and if done queries contained votes for new public address, return the address
    /// to be pinged.
    pub(crate) fn cleanup_done_queries(
        &mut self,
        done_get_queries: &[(Id, Box<[Node]>)],
        done_put_queries: &[(Id, Option<PutError>)],
    ) -> (Vec<Id>, Option<SocketAddrV4>) {
        let mut should_start_put_queries = Vec::with_capacity(done_get_queries.len());
        let mut should_ping_alleged_new_address = None;

        for (id, closest_nodes) in done_get_queries {
            if let Some(query) = self.iterative_queries.remove(id) {
                should_ping_alleged_new_address =
                    self.update_address_votes_from_iterative_query(&query);
                let relevant_routing_table = self.cache_iterative_query(&query, closest_nodes);

                // Only for get queries, not find node.
                if !matches!(query.request.request_type, RequestTypeSpecific::FindNode(_)) {
                    debug!(
                        target = ?query.target(),
                        responders_size_estimate = ?relevant_routing_table.responders_based_dht_size_estimate(),
                        responders_subnets_count = ?relevant_routing_table.average_subnets(),
                        "Storing nodes stats..",
                    );

                    if let Some(put_query) = self.put_queries.get_mut(id) {
                        if !put_query.started() {
                            should_start_put_queries.push(*id);
                        }
                    }
                }
            };
        }

        for (id, _) in done_put_queries {
            self.put_queries.remove(id);
        }

        (should_start_put_queries, should_ping_alleged_new_address)
    }

    fn update_address_votes_from_iterative_query(
        &mut self,
        query: &IterativeQuery,
    ) -> Option<SocketAddrV4> {
        if let Some(new_address) = query.best_address() {
            self.public_address = Some(new_address);

            if self.public_address.is_none()
                || new_address
                    != self
                        .public_address
                        .expect("self.public_address is not None")
            {
                trace!(
                    ?new_address,
                    "Query responses suggest a different public_address, trying to confirm.."
                );

                self.firewalled = true;
                return Some(new_address);
            }
        }

        None
    }

    fn cache_iterative_query<'a>(
        &'a mut self,
        query: &'a IterativeQuery,
        closest_responding_nodes: &'a [Node],
    ) -> &'a RoutingTable {
        if self.cached_iterative_queries.len() >= MAX_CACHED_ITERATIVE_QUERIES {
            let q = self.cached_iterative_queries.pop_lru();
            self.decrement_cached_iterative_query_stats(q.map(|q| q.1));
        }

        let closest = query.closest();
        let responders = query.responders();

        if closest.nodes().is_empty() {
            // We are clearly offline.
            return &self.routing_table;
        }

        let dht_size_estimate = closest.dht_size_estimate();
        let responders_dht_size_estimate = responders.dht_size_estimate();
        let subnets_count = responders.subnets_count();

        let previous = self.cached_iterative_queries.put(
            query.target(),
            CachedIterativeQuery {
                closest_responding_nodes: closest_responding_nodes.into(),
                dht_size_estimate,
                responders_dht_size_estimate,
                subnets: subnets_count,

                request_type: query.request.request_type.clone(),
            },
        );

        self.decrement_cached_iterative_query_stats(previous);

        let relevant_routing_table = choose_relevant_routing_table_mut(
            &query.request.request_type,
            &mut self.routing_table,
            &mut self.signed_peers_routing_table,
        );

        relevant_routing_table.increment_responders_stats(
            dht_size_estimate,
            responders_dht_size_estimate,
            subnets_count,
        );

        relevant_routing_table
    }

    /// Decrement stats after an iterative query is popped
    fn decrement_cached_iterative_query_stats(&mut self, query: Option<CachedIterativeQuery>) {
        if let Some(CachedIterativeQuery {
            dht_size_estimate,
            responders_dht_size_estimate,
            subnets,
            request_type,
            ..
        }) = query
        {
            match request_type {
                RequestTypeSpecific::FindNode(..) => {
                    self.routing_table
                        .decrement_dht_size_estimate(dht_size_estimate);
                }
                _ => {
                    let relevant_routing_table = choose_relevant_routing_table_mut(
                        &request_type,
                        &mut self.routing_table,
                        &mut self.signed_peers_routing_table,
                    );

                    relevant_routing_table.decrement_responders_stats(
                        dht_size_estimate,
                        responders_dht_size_estimate,
                        subnets,
                    );
                }
            }
        };
    }
}

fn choose_relevant_routing_table_mut<'a>(
    request_type: &'a RequestTypeSpecific,
    basic_routing_table: &'a mut RoutingTable,
    signed_peers_routing_table: &'a mut RoutingTable,
) -> &'a mut RoutingTable {
    match request_type {
        RequestTypeSpecific::GetSignedPeers(_) => signed_peers_routing_table,
        _ => basic_routing_table,
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
