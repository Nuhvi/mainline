use std::collections::HashMap;
use std::net::SocketAddrV4;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use lru::LruCache;
use tracing::error;
use tracing::{debug, trace};

use iterative_query::IterativeQuery;
use put_query::PutQuery;

use crate::common::{
    Id, Node, RequestTypeSpecific, RoutingTable, SignedAnnounce, MAX_BUCKET_SIZE_K,
};
use crate::core::iterative_query::GetRequestSpecific;
use crate::{MutableItem, PutRequestSpecific};

pub(crate) mod handle_request;
pub(crate) mod handle_response;
pub(crate) mod iterative_query;
pub(crate) mod put_query;
pub(crate) mod server;

use server::Server;
use server::ServerSettings;

pub use put_query::{ConcurrencyError, PutError, PutQueryError};

pub(crate) const REFRESH_TABLE_INTERVAL: Duration = Duration::from_mins(15);
pub(crate) const PING_TABLE_INTERVAL: Duration = Duration::from_mins(5);

pub(crate) const MAX_CACHED_ITERATIVE_QUERIES: usize = 1000;

const MAX_CACHED_ITERATIVE_QUERIES_NONZERO: NonZeroUsize =
    NonZeroUsize::new(MAX_CACHED_ITERATIVE_QUERIES).unwrap();

/// How many requesters we remember having pinged.
const MAX_RECENTLY_PINGED: usize = 1000;
const MAX_RECENTLY_PINGED_NONZERO: NonZeroUsize = NonZeroUsize::new(MAX_RECENTLY_PINGED).unwrap();
/// Minimum time before we ping the same requester again.
pub(crate) const REQUESTER_PING_INTERVAL: Duration = Duration::from_secs(10);

pub(crate) const VERSION: [u8; 4] = [82, 83, 0, 6]; // "RS" version 06
                                                    //
pub(crate) const VERSIONS_SUPPORTING_SIGNED_PEERS: &[[u8; 4]] = &[
    // This node
    VERSION,
    // Add more nodes as we learn about supporting clients
    // b"LT.."
];

/// Identifies an internal operation, independently of its DHT target.
pub(crate) type QueryId = u64;

#[derive(Debug)]
/// Side effect free Core, containing all the state and methods necessary
/// for comprehensive and deterministic testing.
pub(crate) struct Core {
    // Options
    pub bootstrap: Box<[SocketAddrV4]>,

    // Routing
    /// Closest nodes to this node
    pub routing_table: RoutingTable,
    /// Closest nodes to this node that support the signed peers
    /// [BEP_????](https://github.com/Nuhvi/mainline/blob/main/beps/bep_signed_peers.rst) proposal.
    pub signed_peers_routing_table: RoutingTable,

    /// Closest responding nodes to specific target
    pub cached_iterative_queries: LruCache<Id, CachedIterativeQuery>,
    /// Active `IterativeQueries`
    pub iterative_queries: HashMap<QueryId, IterativeQuery>,
    /// Put queries are special, since they have to wait for a corresponding
    /// get query to finish, update the `closest_nodes`, then `query_all` these.
    pub put_queries: HashMap<QueryId, PutQuery>,
    next_query_id: QueryId,

    // Time
    /// Last time we refreshed the routing table with a `find_node` query.
    pub last_table_refresh: Instant,
    /// Last time we pinged nodes in the routing table.
    pub last_table_ping: Instant,

    pub server: Server,
    pub public_address: Option<SocketAddrV4>,
    pub firewalled: bool,
    pub server_mode: bool,

    /// Requesters to ping, so that their responses can add them to the routing tables.
    pending_pings: Vec<SocketAddrV4>,
    /// When we last pinged a requester, to not ping it for every request.
    recently_pinged: LruCache<SocketAddrV4, Instant>,
}

impl Core {
    pub(crate) fn new(
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
            next_query_id: 0,
            cached_iterative_queries: LruCache::new(MAX_CACHED_ITERATIVE_QUERIES_NONZERO),
            last_table_refresh: Instant::now(),
            last_table_ping: Instant::now(),
            server: Server::new(server_settings),
            public_address: None,
            firewalled: true,
            server_mode,
            pending_pings: Vec::new(),
            recently_pinged: LruCache::new(MAX_RECENTLY_PINGED_NONZERO),
        }
    }

    fn id(&self) -> &Id {
        self.routing_table.id()
    }

    /// Takes the addresses that should be pinged now.
    pub(crate) fn take_pending_pings(&mut self) -> Vec<SocketAddrV4> {
        std::mem::take(&mut self.pending_pings)
    }

    pub(crate) fn should_ping_table(&self) -> bool {
        self.last_table_ping.elapsed() > PING_TABLE_INTERVAL
    }

    pub(crate) fn update_last_table_ping(&mut self) {
        self.last_table_ping = Instant::now();
    }

    pub(crate) fn should_refresh_table(&self) -> bool {
        self.last_table_refresh.elapsed() > REFRESH_TABLE_INTERVAL
    }

    pub(crate) fn update_last_table_refresh(&mut self) {
        self.last_table_refresh = Instant::now();
    }

    pub(crate) fn get_cached_closest_nodes(&mut self, target: &Id) -> Option<Box<[Node]>> {
        self.cached_iterative_queries
            .get(target)
            .map(|cached| cached.closest_responding_nodes.clone())
            .filter(|closest_nodes| {
                !closest_nodes.is_empty() && closest_nodes.iter().any(Node::valid_token)
            })
    }

    /// Check if the query is either successfully done, in which case return the closest nodes.
    pub(crate) fn closest_nodes_from_done_iterative_query(
        &self,
        query: &IterativeQuery,
    ) -> Box<[Node]> {
        let self_id = self.id();
        let table_size = self.routing_table.size();

        let closest_nodes = if let RequestTypeSpecific::FindNode(_) = query.request.request_type {
            if query.target() == *self_id {
                if !self.bootstrap.is_empty() && table_size == 0 {
                    error!("Could not bootstrap the routing table");
                } else {
                    debug!(
                        ?self_id,
                        table_size,
                        signed_peers_table_size = self.signed_peers_routing_table.size(),
                        "Populated the routing table"
                    );
                }
            }

            query
                .closest()
                .nodes()
                .iter()
                .take(MAX_BUCKET_SIZE_K)
                .cloned()
                .collect::<Box<[_]>>()
        } else {
            let relevant_routing_table = choose_relevant_routing_table(
                &query.request.request_type,
                &self.routing_table,
                &self.signed_peers_routing_table,
            );

            query
                .responders()
                .take_until_secure(
                    relevant_routing_table.responders_based_dht_size_estimate(),
                    relevant_routing_table.average_subnets(),
                )
                .to_vec()
                .into_boxed_slice()
        };

        closest_nodes
    }

    /// Remove done [`IterativeQuery`]s, return the [Id]s of [`PutQuery`] ready to start,
    /// and if done queries contained votes for new public address, return the address
    /// to be pinged.
    pub(crate) fn cleanup_done_queries(
        &mut self,
        done_get_queries: &[(QueryId, Box<[Node]>)],
        done_put_queries: &[(QueryId, Result<Id, PutError>)],
    ) -> Option<SocketAddrV4> {
        let mut should_ping_alleged_new_address = None;

        for (id, closest_nodes) in done_get_queries {
            if let Some(query) = self.iterative_queries.remove(id) {
                self.cache_iterative_query(&query, closest_nodes);

                should_ping_alleged_new_address =
                    self.update_address_votes_from_iterative_query(&query);
            }
        }

        for (id, _) in done_put_queries {
            self.put_queries.remove(id);
        }

        should_ping_alleged_new_address
    }

    pub(crate) fn check_nodes_to_ping_and_remove_stale_nodes(&mut self) -> Vec<SocketAddrV4> {
        let mut to_ping = vec![];

        for routing_table in [
            &mut self.routing_table,
            &mut self.signed_peers_routing_table,
        ] {
            let mut to_remove = Vec::with_capacity(routing_table.size());

            for node in routing_table.nodes() {
                if node.is_stale() {
                    to_remove.push(*node.id());
                } else if node.should_ping() {
                    to_ping.push(node.address());
                }
            }

            for id in to_remove {
                routing_table.remove(&id);
            }
        }

        to_ping
    }

    /// Check a mutable put against another pending put for the same target.
    ///
    /// Returns the id of an identical pending put to join. Returns an error for a
    /// conflicting put. The caller must remove finished puts before it calls this
    /// method, so a timed-out put cannot block or absorb a new one.
    pub(crate) fn check_pending_put(
        &self,
        incoming: &crate::common::PutMutableRequestArguments,
    ) -> Result<Option<QueryId>, ConcurrencyError> {
        let mut identical = None;
        let mut latest: Option<&crate::common::PutMutableRequestArguments> = None;
        let mut overrides = false;

        for (id, query) in &self.put_queries {
            let PutRequestSpecific::PutMutable(pending) = &query.request else {
                continue;
            };
            if pending.target != incoming.target {
                continue;
            }
            if pending.sig == incoming.sig && pending.cas == incoming.cas {
                identical = Some(*id);
                break;
            }
            if incoming.cas == Some(pending.seq) {
                overrides = true;
            }
            if latest.is_none_or(|latest| pending.seq > latest.seq) {
                latest = Some(pending);
            }
        }

        if let Some(id) = identical {
            return Ok(Some(id));
        }
        if overrides {
            return Ok(None);
        }
        if let Some(pending) = latest {
            if incoming.seq < pending.seq {
                return Err(ConcurrencyError::NotMostRecent);
            }
            if incoming.cas.is_some() {
                return Err(ConcurrencyError::CasFailed);
            }
            return Err(ConcurrencyError::ConflictRisk);
        }
        Ok(None)
    }

    /// If we are already announcing a signed peer or a mutable item, might as well
    /// return it to the request asking for the same target.
    pub(crate) fn check_outgoing_put_request(&self, target: &Id) -> Option<Response> {
        self.put_queries
            .values()
            .filter(|query| query.target == *target)
            .find_map(|existing| match existing.request.clone() {
                PutRequestSpecific::PutMutable(request) => Some(Response::Mutable(request.into())),
                PutRequestSpecific::AnnounceSignedPeer(request) => {
                    Some(Response::SignedPeers(vec![SignedAnnounce {
                        key: request.k,
                        timestamp: request.t,
                        signature: request.sig,
                    }]))
                }

                _ => None,
            })
    }

    /// Reuse an active lookup only when its complete request matches.
    pub(crate) fn find_active_query(
        &self,
        request: &GetRequestSpecific,
    ) -> Option<(QueryId, &[Response])> {
        let request_type = RequestTypeSpecific::from(request.clone());
        self.iterative_queries.iter().find_map(|(id, query)| {
            (query.request.request_type == request_type).then_some((*id, query.responses()))
        })
    }

    /// Create an [`IterativeQuery`] and populate it with candidate nodes, but not insert it to
    /// [`Core::iterative_queries`] yet.
    ///
    /// Return its internal id, the query, and a list of nodes (from our bootstrapping
    /// node and/or routing tables) to visit to start the query.
    ///
    /// The caller may reuse an existing query with [`Self::find_active_query`].
    pub(crate) fn create_iterative_query(
        &mut self,
        request: GetRequestSpecific,
        extra_nodes: Option<&[SocketAddrV4]>,
    ) -> (QueryId, IterativeQuery, Vec<SocketAddrV4>) {
        let target = request.target();

        // We have multiple routing table now, so we should first figure out which one
        // is the appropriate for this query.
        let routing_table_closest = self.get_closest_nodes_from_routing_tables(&request);

        let mut query = IterativeQuery::new(*self.id(), target, request);

        // Seed this query with the closest nodes we know about.
        for node in routing_table_closest {
            query.add_candidate(node);
        }

        // If we have cached iterative query with the same hash, use its nodes as well..
        if let Some(closest_responding_nodes) = self.get_cached_closest_nodes(&target) {
            for node in closest_responding_nodes {
                query.add_candidate(node.clone());
            }
        }

        let mut to_visit = query.initial_candidates();

        // Seed the query either with the closest nodes from the routing table, or the
        // bootstrapping nodes if the closest nodes are not enough.
        let candidates = query.closest();
        if candidates.is_empty() || candidates.len() < self.bootstrap.len() {
            for bootstrapping_node in self.bootstrap.clone() {
                to_visit.push(bootstrapping_node);
            }
        }

        if let Some(extra_nodes) = extra_nodes {
            to_visit.extend_from_slice(extra_nodes);
        }

        (self.allocate_query_id(), query, to_visit)
    }

    /// Allocate an id distinct from every active read lookup.
    pub(crate) fn allocate_query_id(&mut self) -> QueryId {
        loop {
            let id = self.next_query_id;
            self.next_query_id = self.next_query_id.wrapping_add(1);
            if !self.iterative_queries.contains_key(&id) && !self.put_queries.contains_key(&id) {
                // Ids only repeat after the counter wraps, so this normally returns
                // on the first iteration.
                return id;
            }
        }
    }

    // === Private Methods ===

    fn get_closest_nodes_from_routing_tables(&self, request: &GetRequestSpecific) -> Vec<Node> {
        let target = request.target();

        match &request {
            // We don't actually need to use closest secure, because we aren't storing anything
            // to these nodes, but it is better ask further away than we need, just to add more
            // randomness and to more likely defeat eclipsing attempts, but we pay the price in
            // more messages, however that is ok when we rarely call FIND_NODE (once every 15 minutes)
            GetRequestSpecific::FindNode(_) => {
                let mut routing_table_closest = self.routing_table.closest_secure(target);
                routing_table_closest
                    .extend_from_slice(&self.signed_peers_routing_table.closest_secure(target));
                routing_table_closest
            }
            GetRequestSpecific::GetSignedPeers(_) => {
                self.signed_peers_routing_table.closest_secure(target)
            }
            _ => self.routing_table.closest_secure(target),
        }
    }

    fn update_address_votes_from_iterative_query(
        &mut self,
        query: &IterativeQuery,
    ) -> Option<SocketAddrV4> {
        if let Some(new_address) = query.best_address() {
            if self.public_address != Some(new_address) {
                self.public_address = Some(new_address);

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

    fn cache_iterative_query(&mut self, query: &IterativeQuery, closest_responding_nodes: &[Node]) {
        if self.cached_iterative_queries.len() >= MAX_CACHED_ITERATIVE_QUERIES {
            let q = self.cached_iterative_queries.pop_lru();
            self.decrement_cached_iterative_query_stats(q.map(|q| q.1));
        }

        let closest = query.closest();
        let responders = query.responders();

        if closest.nodes().is_empty() {
            // We are clearly offline.
            return;
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

        if matches!(query.request.request_type, RequestTypeSpecific::FindNode(_)) {
            relevant_routing_table.increment_dht_size_estimate(dht_size_estimate);
        } else {
            relevant_routing_table.increment_responders_stats(
                dht_size_estimate,
                responders_dht_size_estimate,
                subnets_count,
            );

            debug!(
                target = ?query.target(),
                responders_size_estimate = ?relevant_routing_table.responders_based_dht_size_estimate(),
                responders_subnets_count = ?relevant_routing_table.average_subnets(),
                "Storing nodes stats..",
            );
        }
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
            if let RequestTypeSpecific::FindNode(..) = request_type {
                self.routing_table
                    .decrement_dht_size_estimate(dht_size_estimate);
            } else {
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

fn choose_relevant_routing_table<'a>(
    request_type: &RequestTypeSpecific,
    basic_routing_table: &'a RoutingTable,
    signed_peers_routing_table: &'a RoutingTable,
) -> &'a RoutingTable {
    match request_type {
        RequestTypeSpecific::GetSignedPeers(_) => signed_peers_routing_table,
        _ => basic_routing_table,
    }
}

fn supports_signed_peers(version: Option<[u8; 4]>) -> bool {
    version.is_some_and(|version| {
        VERSIONS_SUPPORTING_SIGNED_PEERS
            .iter()
            .any(|v| version[0..2] == v[0..2] && version[2..] >= v[2..])
    })
}

pub(crate) struct CachedIterativeQuery {
    closest_responding_nodes: Box<[Node]>,
    dht_size_estimate: f64,
    responders_dht_size_estimate: f64,
    subnets: u8,

    request_type: RequestTypeSpecific,
}

#[derive(Debug, Clone)]
pub enum Response {
    Peers(Vec<SocketAddrV4>),
    SignedPeers(Vec<SignedAnnounce>),
    Immutable(Box<[u8]>),
    Mutable(MutableItem),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{FindNodeRequestArguments, GetValueRequestArguments};
    use crate::core::iterative_query::GetRequestSpecific;

    fn node() -> Node {
        Node::new(
            Id::from([2; 20]),
            SocketAddrV4::new([198, 51, 100, 1].into(), 6881),
        )
    }

    fn mutable_request(value: &[u8], seq: i64, cas: Option<i64>) -> PutRequestSpecific {
        let signer = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let item = MutableItem::new(&signer, value, seq, None);
        PutRequestSpecific::PutMutable(crate::common::PutMutableRequestArguments::from(item, cas))
    }

    #[test]
    fn identical_pending_put_is_joined() {
        let mut core = Core::new(Id::random(), vec![], false, ServerSettings::default());
        let request = mutable_request(b"value", 2, None);
        let id = core.allocate_query_id();
        core.put_queries
            .insert(id, PutQuery::new(request.clone(), None));

        let PutRequestSpecific::PutMutable(incoming) = &request else {
            return;
        };
        assert_eq!(core.check_pending_put(incoming), Ok(Some(id)));
    }

    #[test]
    fn pending_put_conflicts_are_rejected() {
        let mut core = Core::new(Id::random(), vec![], false, ServerSettings::default());
        let id = core.allocate_query_id();
        core.put_queries
            .insert(id, PutQuery::new(mutable_request(b"a", 2, None), None));

        let check = |request: PutRequestSpecific| {
            let PutRequestSpecific::PutMutable(incoming) = &request else {
                return Ok(None);
            };
            core.check_pending_put(incoming)
        };

        assert_eq!(
            check(mutable_request(b"a", 1, None)),
            Err(ConcurrencyError::NotMostRecent)
        );
        assert_eq!(
            check(mutable_request(b"b", 2, None)),
            Err(ConcurrencyError::ConflictRisk)
        );
        assert_eq!(
            check(mutable_request(b"b", 3, Some(1))),
            Err(ConcurrencyError::CasFailed)
        );
        assert_eq!(core.put_queries.len(), 1);
    }

    #[test]
    fn pending_put_can_be_overridden_with_its_seq() {
        let mut core = Core::new(Id::random(), vec![], false, ServerSettings::default());
        let id = core.allocate_query_id();
        core.put_queries
            .insert(id, PutQuery::new(mutable_request(b"a", 2, None), None));

        let PutRequestSpecific::PutMutable(incoming) = mutable_request(b"b", 3, Some(2)) else {
            return;
        };
        assert_eq!(core.check_pending_put(&incoming), Ok(None));
    }

    #[test]
    fn query_ids_skip_active_ids_after_wrap() {
        let mut core = Core::new(Id::random(), vec![], false, ServerSettings::default());
        core.put_queries
            .insert(0, PutQuery::new(mutable_request(b"a", 1, None), None));
        core.next_query_id = u64::MAX;

        assert_eq!(core.allocate_query_id(), u64::MAX);
        assert_eq!(core.allocate_query_id(), 1);
    }

    #[test]
    fn find_node_does_not_change_responder_stats() {
        let mut core = Core::new(Id::random(), vec![], false, ServerSettings::default());
        let node = node();
        let get_target = Id::from([1; 20]);

        let mut get = IterativeQuery::new(
            *core.id(),
            get_target,
            GetRequestSpecific::GetValue(GetValueRequestArguments {
                target: get_target,
                seq: None,
                salt: None,
            }),
        );
        get.add_candidate(node.clone());
        get.add_responding_node(node.clone());
        core.cache_iterative_query(&get, std::slice::from_ref(&node));

        let responders = core.routing_table.responders_based_dht_size_estimate();
        assert!(responders > 0);

        let find_target = Id::from([3; 20]);
        let mut find = IterativeQuery::new(
            *core.id(),
            find_target,
            GetRequestSpecific::FindNode(FindNodeRequestArguments {
                target: find_target,
            }),
        );
        find.add_candidate(node.clone());
        core.cache_iterative_query(&find, std::slice::from_ref(&node));

        assert_eq!(
            core.routing_table.responders_based_dht_size_estimate(),
            responders,
            "a find_node query changed the responder statistics"
        );

        // Replacing the cached find_node entry must not change them either.
        core.cache_iterative_query(&find, std::slice::from_ref(&node));
        assert_eq!(
            core.routing_table.responders_based_dht_size_estimate(),
            responders
        );
    }
}
