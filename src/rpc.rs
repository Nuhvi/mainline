//! K-RPC implementation.

pub(crate) mod config;
mod info;
pub(crate) mod socket;

use std::collections::HashSet;
use std::net::{SocketAddr, SocketAddrV4, ToSocketAddrs};
use std::time::{Duration, Instant};

use tracing::{debug, error, info, trace};

use crate::common::{
    messages::{GetPeersRequestArguments, PutMutableRequestArguments},
    validate_immutable, FindNodeRequestArguments, GetImmutableResponseArguments,
    GetMutableResponseArguments, GetPeersResponseArguments, GetSignedPeersResponseArguments,
    GetValueRequestArguments, Id, Message, MessageType, MutableItem,
    NoMoreRecentValueResponseArguments, NoValuesResponseArguments, Node, PutRequestSpecific,
    RequestSpecific, RequestTypeSpecific, ResponseSpecific, RoutingTable, SignedAnnounce,
    MAX_BUCKET_SIZE_K,
};
use crate::core::iterative_query::GetRequestSpecific;
use crate::core::{
    iterative_query::IterativeQuery, put_query::PutQuery, supports_signed_peers, Core,
};
use crate::core::{CachedIterativeQuery, ConcurrencyError, PutError};

use socket::KrpcSocket;

pub use info::Info;

pub(crate) const REFRESH_TABLE_INTERVAL: Duration = Duration::from_secs(15 * 60);
pub(crate) const PING_TABLE_INTERVAL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug)]
/// Internal Rpc called in the Dht thread loop, useful to create your own actor setup.
pub struct Rpc {
    socket: KrpcSocket,
    core: Core,

    /// Last time we refreshed the routing table with a find_node query.
    pub(crate) last_table_refresh: Instant,
    /// Last time we pinged nodes in the routing table.
    pub(crate) last_table_ping: Instant,
}

impl Rpc {
    /// Create a new Rpc
    pub fn new(config: config::Config) -> Result<Self, std::io::Error> {
        let id = if let Some(ip) = config.public_ip {
            Id::from_ip(ip.into())
        } else {
            Id::random()
        };

        let socket = KrpcSocket::new(&config)?;
        let bootstrap = to_socket_address(&config.bootstrap);

        Ok(Rpc {
            socket,
            core: Core::new(id, bootstrap, config.server_mode, config.server_settings),
            last_table_refresh: Instant::now(),
            last_table_ping: Instant::now(),
        })
    }

    // === Getters ===

    /// Returns the node's Id
    pub fn id(&self) -> &Id {
        self.core.routing_table.id()
    }

    /// Returns the address the server is listening to.
    #[inline]
    pub fn local_addr(&self) -> SocketAddrV4 {
        self.socket.local_addr()
    }

    /// Returns the best guess for this node's Public address.
    ///
    /// If [crate::DhtBuilder::public_ip] was set, this is what will be returned
    /// (plus the local port), otherwise it will rely on consensus from
    /// responding nodes voting on our public IP and port.
    pub fn public_address(&self) -> Option<SocketAddrV4> {
        self.core.public_address
    }

    /// Returns `true` if we can't confirm that [Self::public_address] is publicly addressable.
    ///
    /// If this node is firewalled, it won't switch to server mode if it is in adaptive mode,
    /// but if [crate::DhtBuilder::server_mode] was set to true, then whether or not this node is firewalled
    /// won't matter.
    pub fn firewalled(&self) -> bool {
        self.core.firewalled
    }

    /// Returns whether or not this node is running in server mode.
    pub fn server_mode(&self) -> bool {
        self.socket.server_mode
    }

    pub fn routing_table(&self) -> &RoutingTable {
        &self.core.routing_table
    }

    /// Create a list of unique bootstrapping nodes from all our
    /// routing table to use as `extra_bootsrtap` in next sessions.
    pub fn to_bootstrap(&self) -> Vec<String> {
        let mut set = HashSet::new();
        for s in self.routing_table().to_bootstrap() {
            set.insert(s);
        }
        for s in self.core.signed_peers_routing_table.to_bootstrap() {
            set.insert(s);
        }

        set.iter().cloned().collect()
    }

    /// Returns:
    ///  1. Normal Dht size estimate based on all closer `nodes` in query responses.
    ///  2. Standard deviaiton as a function of the number of samples used in this estimate.
    ///
    /// [Read more](https://github.com/nuhvi/mainline/blob/main/docs/dht_size_estimate.md)
    pub fn dht_size_estimate(&self) -> (usize, f64) {
        self.core.routing_table.dht_size_estimate()
    }

    /// Returns a thread safe and lightweight summary of this node's
    /// information and statistics.
    pub fn info(&self) -> Info {
        Info::from(self)
    }

    // === Public Methods ===

    /// Advance the inflight queries, receive incoming requests,
    /// maintain the routing table, and everything else that needs
    /// to happen at every tick.
    pub fn tick(&mut self) -> RpcTickReport {
        // === Periodic node maintaenance ===
        self.periodic_node_maintaenance();

        // === Handle new incoming message ===
        let new_query_response = self
            .socket
            .recv_from()
            .and_then(|(message, from)| self.handle_incoming_message(message, from));

        let mut done_get_queries = Vec::with_capacity(self.core.iterative_queries.len());
        let mut done_put_queries = Vec::with_capacity(self.core.put_queries.len());

        // === Tick Queries ===

        for (id, query) in self.core.put_queries.iter_mut() {
            match query.tick(&self.socket) {
                Ok(done) => {
                    if done {
                        done_put_queries.push((*id, None));
                    }
                }
                Err(error) => done_put_queries.push((*id, Some(error))),
            };
        }

        let self_id = *self.id();
        let basic_routing_table = &self.core.routing_table;
        let signed_peers_routing_table = &self.core.routing_table;

        for (id, query) in self.core.iterative_queries.iter_mut() {
            let is_done = query.tick(&mut self.socket);

            if is_done {
                let closest_nodes = if let RequestTypeSpecific::FindNode(_) =
                    query.request.request_type
                {
                    let table_size = self.core.routing_table.size();
                    let signed_peers_table_size = self.core.signed_peers_routing_table.size();

                    if *id == self_id {
                        if !self.core.bootstrap.is_empty() && table_size == 0 {
                            error!("Could not bootstrap the routing table");
                        } else {
                            debug!(
                                ?self_id,
                                table_size, signed_peers_table_size, "Populated the routing table"
                            );
                        }
                    };

                    query
                        .closest()
                        .nodes()
                        .iter()
                        .take(MAX_BUCKET_SIZE_K)
                        .cloned()
                        .collect::<Box<[_]>>()
                } else {
                    let relevant_routing_table = choose_relevant_routing_table(
                        query.request.request_type.clone(),
                        basic_routing_table,
                        signed_peers_routing_table,
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

                done_get_queries.push((*id, closest_nodes));
            };
        }

        // === Cleanup done queries ===

        let (should_start_put_queries, should_ping_alleged_new_address) = self
            .core
            .cleanup_done_queries(&done_get_queries, &done_put_queries);

        if let Some(address) = should_ping_alleged_new_address {
            self.ping(address);
        }

        for id in should_start_put_queries {
            let put_query = self
                .core
                .put_queries
                .get_mut(&id)
                .expect("put query shouldn't be deleted before done..");

            let (_, closest_nodes) = done_get_queries
                .iter()
                .find(|(this_id, _)| this_id == &id)
                .expect("done_get_queries");

            if let Err(error) = put_query.start(&mut self.socket, closest_nodes) {
                done_put_queries.push((id, Some(error)))
            }
        }

        RpcTickReport {
            done_get_queries,
            done_put_queries,
            new_query_response,
        }
    }

    fn handle_incoming_message(
        &mut self,
        message: Message,
        from: SocketAddrV4,
    ) -> Option<(Id, Response)> {
        match message.message_type {
            MessageType::Request(request_specific) => {
                let (response, should_repopulate_routing_tables) = self.core.handle_request(
                    from,
                    message.read_only,
                    message.version,
                    request_specific,
                );

                match response {
                    Some(MessageType::Error(error)) => {
                        self.socket.error(from, message.transaction_id, error)
                    }
                    Some(MessageType::Response(response)) => {
                        self.socket.response(from, message.transaction_id, response)
                    }
                    _ => {}
                }

                if should_repopulate_routing_tables {
                    self.populate();
                }

                None
            }
            _ => self.handle_response(from, message),
        }
    }

    /// Store a value in the closest nodes, optionally trigger a lookup query if
    /// the cached closest_nodes aren't fresh enough.
    ///
    /// - `request`: the put request.
    pub fn put(
        &mut self,
        request: PutRequestSpecific,
        extra_nodes: Option<Box<[Node]>>,
    ) -> Result<(), PutError> {
        let target = *request.target();

        if let PutRequestSpecific::PutMutable(PutMutableRequestArguments {
            sig, cas, seq, ..
        }) = &request
        {
            if let Some(PutRequestSpecific::PutMutable(inflight_request)) = self
                .core
                .put_queries
                .get(&target)
                .map(|existing| &existing.request)
            {
                debug!(?inflight_request, ?request, "Possible conflict risk");

                if *sig == inflight_request.sig {
                    // Noop, the inflight query is sufficient.
                    return Ok(());
                } else if *seq < inflight_request.seq {
                    return Err(ConcurrencyError::NotMostRecent)?;
                } else if let Some(cas) = cas {
                    if *cas == inflight_request.seq {
                        // The user is aware of the inflight query and whiches to overrides it.
                        //
                        // Remove the inflight request, and create a new one.
                        self.core.put_queries.remove(&target);
                    } else {
                        return Err(ConcurrencyError::CasFailed)?;
                    }
                } else {
                    return Err(ConcurrencyError::ConflictRisk)?;
                };
            };
        }

        let mut query = PutQuery::new(target, request.clone(), extra_nodes);

        if let Some(closest_nodes) = self
            .core
            .cached_iterative_queries
            .get(&target)
            .map(|cached| cached.closest_responding_nodes.clone())
            .filter(|closest_nodes| {
                !closest_nodes.is_empty() && closest_nodes.iter().any(|n| n.valid_token())
            })
        {
            query.start(&mut self.socket, &closest_nodes)?
        } else {
            let get_request = match request {
                PutRequestSpecific::PutImmutable(_) => {
                    GetRequestSpecific::GetValue(GetValueRequestArguments {
                        target,
                        seq: None,
                        salt: None,
                    })
                }
                PutRequestSpecific::PutMutable(args) => {
                    GetRequestSpecific::GetValue(GetValueRequestArguments {
                        target,
                        seq: None,
                        salt: args.salt,
                    })
                }
                PutRequestSpecific::AnnouncePeer(_) => {
                    GetRequestSpecific::GetPeers(GetPeersRequestArguments { info_hash: target })
                }
                PutRequestSpecific::AnnounceSignedPeer(_) => {
                    GetRequestSpecific::GetSignedPeers(GetPeersRequestArguments {
                        info_hash: target,
                    })
                }
            };

            self.get(get_request, None);
        };

        self.core.put_queries.insert(target, query);

        Ok(())
    }

    /// Send a message to closer and closer nodes until we can't find any more nodes.
    ///
    /// Queries take few seconds to fully traverse the network, once it is done, it will be removed from
    /// self.iterative_queries. But until then, calling [Rpc::get] multiple times, will just return the list
    /// of responses seen so far.
    ///
    /// Subsequent responses can be obtained from the [RpcTickReport::new_query_response] you get after calling [Rpc::tick].
    ///
    /// Effectively, we are caching responses and backing off the network for the duration it takes
    /// to traverse it.
    ///
    /// - `request` [RequestTypeSpecific], except [RequestTypeSpecific::Ping] and
    ///   [RequestTypeSpecific::Put] which will be ignored.
    /// - `extra_nodes` option allows the query to visit specific nodes, that won't necessesarily be visited
    ///   through the query otherwise.
    pub fn get(
        &mut self,
        request: GetRequestSpecific,
        extra_nodes: Option<&[SocketAddrV4]>,
    ) -> Option<Vec<Response>> {
        let target = match request {
            GetRequestSpecific::FindNode(FindNodeRequestArguments { target }) => target,
            GetRequestSpecific::GetPeers(GetPeersRequestArguments { info_hash, .. }) => info_hash,
            GetRequestSpecific::GetSignedPeers(GetPeersRequestArguments { info_hash, .. }) => {
                info_hash
            }
            GetRequestSpecific::GetValue(GetValueRequestArguments { target, .. }) => target,
        };

        let response_from_inflight_put_mutable_request =
            self.core.put_queries.get(&target).and_then(|existing| {
                if let PutRequestSpecific::PutMutable(request) = &existing.request {
                    Some(Response::Mutable(request.clone().into()))
                } else {
                    None
                }
            });

        // If query is still active, no need to create a new one.
        if let Some(query) = self.core.iterative_queries.get(&target) {
            let mut responses = query.responses().to_vec();

            if let Some(response) = response_from_inflight_put_mutable_request {
                responses.push(response);
            }

            return Some(responses);
        }

        let node_id = self.core.routing_table.id();

        if target == *node_id {
            debug!(?node_id, "Bootstrapping the routing table");
        }

        // We have multiple routing table now, so we should first figure out which one
        // is the appropriate for this query.
        let routing_table_closest = match &request {
            // We don't actually need to use closest secure, because we aren't storing anything
            // to these nodes, but it is better ask further away than we need, just to add more
            // randomness and to more likely defeat eclipsing attempts, but we pay the price in
            // more messages, however that is ok when we rarely call FIND_NODE (once every 15 minutes)
            GetRequestSpecific::FindNode(_) => {
                let mut routing_table_closest = self.core.routing_table.closest_secure(target);
                routing_table_closest.extend_from_slice(
                    &self.core.signed_peers_routing_table.closest_secure(target),
                );
                routing_table_closest
            }
            GetRequestSpecific::GetSignedPeers(_) => {
                self.core.signed_peers_routing_table.closest_secure(target)
            }
            _ => self.core.routing_table.closest_secure(target),
        };

        let mut query = IterativeQuery::new(*self.id(), target, request);

        // Seed the query either with the closest nodes from the routing table, or the
        // bootstrapping nodes if the closest nodes are not enough.
        if routing_table_closest.is_empty()
            || routing_table_closest.len() < self.core.bootstrap.len()
        {
            for bootstrapping_node in self.core.bootstrap.clone() {
                query.visit(&mut self.socket, bootstrapping_node);
            }
        }

        if let Some(extra_nodes) = extra_nodes {
            for extra_node in extra_nodes {
                query.visit(&mut self.socket, *extra_node)
            }
        }

        // Seed this query with the closest nodes we know about.
        for node in routing_table_closest {
            query.add_candidate(node)
        }

        // If we have cached iterative query with the same hash,
        // use its nodes as well..
        if let Some(CachedIterativeQuery {
            closest_responding_nodes,
            ..
        }) = self.core.cached_iterative_queries.get(&target)
        {
            for node in closest_responding_nodes {
                query.add_candidate(node.clone())
            }
        }

        // After adding the nodes, we need to start the query.
        query.start(&mut self.socket);

        self.core.iterative_queries.insert(target, query);

        // If there is an inflight PutQuery for mutable item return its value
        if let Some(response) = response_from_inflight_put_mutable_request {
            return Some(vec![response]);
        }

        None
    }

    // === Private Methods ===

    fn handle_response(&mut self, from: SocketAddrV4, message: Message) -> Option<(Id, Response)> {
        // If someone claims to be readonly, then let's not store anything even if they respond.
        if message.read_only {
            return None;
        };

        // If the response looks like a Ping response, check StoreQueries for the transaction_id.
        if let Some(query) = self
            .core
            .put_queries
            .values_mut()
            .find(|query| query.inflight(message.transaction_id))
        {
            match message.message_type {
                MessageType::Response(ResponseSpecific::Ping(_)) => {
                    // Mark storage at that node as a success.
                    query.success();
                }
                MessageType::Error(error) => query.error(error),
                _ => {}
            };

            return None;
        }

        let mut should_add_node = false;
        let author_id = message.get_author_id();
        let from_version = message.version.to_owned();

        // Get corresponding query for message.transaction_id
        if let Some(query) = self
            .core
            .iterative_queries
            .values_mut()
            .find(|query| query.inflight(message.transaction_id))
        {
            // KrpcSocket would not give us a response from the wrong address for the transaction_id
            should_add_node = true;

            if let Some(nodes) = message.get_closer_nodes() {
                for node in nodes {
                    query.add_candidate(node.clone());
                }
            }

            if let Some((responder_id, token)) = message.get_token() {
                query.add_responding_node(Node::new_with_token(responder_id, from, token.into()));
            }

            if let Some(proposed_ip) = message.requester_ip {
                query.add_address_vote(proposed_ip);
            }

            let target = query.target();

            match message.message_type {
                MessageType::Response(ResponseSpecific::GetPeers(GetPeersResponseArguments {
                    values,
                    ..
                })) => {
                    let response = Response::Peers(values);
                    query.response(from, response.clone());

                    return Some((target, response));
                }
                MessageType::Response(ResponseSpecific::GetSignedPeers(
                    GetSignedPeersResponseArguments {
                        responder_id,
                        peers,
                        ..
                    },
                )) => {
                    let mut verified_peers = vec![];
                    let mut malicious = false;

                    for (k, t, sig) in peers {
                        if let Ok(peer) = SignedAnnounce::from_dht_response(&target, &k, t, &sig) {
                            verified_peers.push(peer);
                        } else {
                            malicious = true;
                            break;
                        }
                    }

                    if malicious {
                        debug!(
                            ?from,
                            ?responder_id,
                            ?from_version,
                            "Invalid signed announce record"
                        );

                        should_add_node = false;
                    } else {
                        let response = Response::SignedPeers(verified_peers);
                        query.response(from, response.clone());

                        return Some((target, response));
                    }
                }
                MessageType::Response(ResponseSpecific::GetImmutable(
                    GetImmutableResponseArguments {
                        v, responder_id, ..
                    },
                )) => {
                    if validate_immutable(&v, query.target()) {
                        let response = Response::Immutable(v);
                        query.response(from, response.clone());

                        return Some((target, response));
                    }

                    let target = query.target();
                    debug!(
                        ?v,
                        ?target,
                        ?responder_id,
                        ?from,
                        ?from_version,
                        "Invalid immutable value"
                    );
                }
                MessageType::Response(ResponseSpecific::GetMutable(
                    GetMutableResponseArguments {
                        v,
                        seq,
                        sig,
                        k,
                        responder_id,
                        ..
                    },
                )) => {
                    let salt = match query.request.request_type.clone() {
                        RequestTypeSpecific::GetValue(args) => args.salt,
                        _ => None,
                    };
                    let target = query.target();

                    match MutableItem::from_dht_message(query.target(), &k, v, seq, &sig, salt) {
                        Ok(item) => {
                            let response = Response::Mutable(item);
                            query.response(from, response.clone());

                            return Some((target, response));
                        }
                        Err(error) => {
                            debug!(
                                ?error,
                                ?from,
                                ?responder_id,
                                ?from_version,
                                "Invalid mutable record"
                            );
                        }
                    }
                }
                MessageType::Response(ResponseSpecific::NoMoreRecentValue(
                    NoMoreRecentValueResponseArguments {
                        seq, responder_id, ..
                    },
                )) => {
                    trace!(
                        target= ?query.target(),
                        salt= ?match query.request.request_type.clone() {
                            RequestTypeSpecific::GetValue(args) => args.salt,
                            _ => None,
                        },
                        ?seq,
                        ?from,
                        ?responder_id,
                        ?from_version,
                        "No more recent value"
                    );
                }
                MessageType::Response(ResponseSpecific::NoValues(NoValuesResponseArguments {
                    responder_id,
                    ..
                })) => {
                    trace!(
                        target= ?query.target(),
                        salt= ?match query.request.request_type.clone() {
                            RequestTypeSpecific::GetValue(args) => args.salt,
                            _ => None,
                        },
                        ?from,
                        ?responder_id,
                        ?from_version ,
                        "No values"
                    );
                }
                MessageType::Error(error) => {
                    debug!(?error, ?from_version, "Get query got error response");
                }
                // Ping response is already handled in add_node()
                // FindNode response is already handled in query.add_candidate()
                // Requests are handled elsewhere
                MessageType::Response(ResponseSpecific::Ping(_))
                | MessageType::Response(ResponseSpecific::FindNode(_))
                | MessageType::Request(_) => {}
            };
        };

        if should_add_node {
            // Add a node to our routing table on any expected incoming response.

            if let Some(id) = author_id {
                self.core.routing_table.add(Node::new(id, from));

                if supports_signed_peers(message.version) {
                    self.core
                        .signed_peers_routing_table
                        .add(Node::new(id, from));
                }
            }
        }

        None
    }

    fn periodic_node_maintaenance(&mut self) {
        // Bootstrap if necessary
        if self.core.routing_table.is_empty() {
            self.populate();
        }

        // Every 15 minutes refresh the routing table.
        if self.last_table_refresh.elapsed() > REFRESH_TABLE_INTERVAL {
            self.last_table_refresh = Instant::now();

            if !self.server_mode() && !self.firewalled() {
                info!("Adaptive mode: have been running long enough (not firewalled), switching to server mode");

                self.socket.server_mode = true;
            }

            self.populate();
        }

        if self.last_table_ping.elapsed() > PING_TABLE_INTERVAL {
            self.last_table_ping = Instant::now();

            let mut to_ping = vec![];

            for routing_table in [
                &mut self.core.routing_table,
                &mut self.core.signed_peers_routing_table,
            ] {
                let mut to_remove = Vec::with_capacity(routing_table.size());

                for node in routing_table.nodes() {
                    if node.is_stale() {
                        to_remove.push(*node.id())
                    } else if node.should_ping() {
                        to_ping.push(node.address())
                    }
                }

                for id in to_remove {
                    routing_table.remove(&id);
                }
            }

            for address in to_ping {
                self.ping(address);
            }
        }
    }

    /// Ping bootstrap nodes, add them to the routing table with closest query.
    fn populate(&mut self) {
        if self.core.bootstrap.is_empty() {
            return;
        }

        self.get(
            GetRequestSpecific::FindNode(FindNodeRequestArguments { target: *self.id() }),
            None,
        );
    }

    fn ping(&mut self, address: SocketAddrV4) {
        self.socket.request(
            address,
            RequestSpecific {
                requester_id: *self.id(),
                request_type: RequestTypeSpecific::Ping,
            },
        );
    }
}

fn choose_relevant_routing_table<'a>(
    request_type: RequestTypeSpecific,
    basic_routing_table: &'a RoutingTable,
    signed_peers_routing_table: &'a RoutingTable,
) -> &'a RoutingTable {
    match request_type {
        RequestTypeSpecific::GetSignedPeers(_) => signed_peers_routing_table,
        _ => basic_routing_table,
    }
}

/// State change after a call to [Rpc::tick], including
/// done PUT, GET, and FIND_NODE queries, as well as any
/// incoming value response for any GET query.
#[derive(Debug, Clone)]
pub struct RpcTickReport {
    /// All the [Id]s of the done [Rpc::get] queries.
    pub done_get_queries: Vec<(Id, Box<[Node]>)>,
    /// All the [Id]s of the done [Rpc::put] queries,
    /// and optional [PutError] if the query failed.
    pub done_put_queries: Vec<(Id, Option<PutError>)>,
    /// Received GET query response.
    pub new_query_response: Option<(Id, Response)>,
}

#[derive(Debug, Clone)]
pub enum Response {
    Peers(Vec<SocketAddrV4>),
    SignedPeers(Vec<SignedAnnounce>),
    Immutable(Box<[u8]>),
    Mutable(MutableItem),
}

pub(crate) fn to_socket_address<T: ToSocketAddrs>(bootstrap: &[T]) -> Vec<SocketAddrV4> {
    bootstrap
        .iter()
        .flat_map(|s| {
            s.to_socket_addrs().map(|addrs| {
                addrs
                    .filter_map(|addr| match addr {
                        SocketAddr::V4(addr_v4) => Some(addr_v4),
                        _ => None,
                    })
                    .collect::<Box<[_]>>()
            })
        })
        .flatten()
        .collect()
}
