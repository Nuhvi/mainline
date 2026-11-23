pub(crate) mod config;
mod info;
pub(crate) mod socket;

use std::collections::{HashMap, HashSet};
use std::net::{SocketAddr, SocketAddrV4, ToSocketAddrs};

use flume::{Receiver, Sender, TryRecvError};

use tracing::{debug, info};

use crate::common::{
    FindNodeRequestArguments, Id, Message, MessageType, Node, PutRequestSpecific, RequestSpecific,
    RequestTypeSpecific, SignedAnnounce,
};
use crate::core::iterative_query::GetRequestSpecific;
use crate::core::put_query::PutQuery;
use crate::core::{Core, PutError, QueryId, Response};
use crate::MutableItem;
use config::Config;

use socket::KrpcSocket;

pub(crate) use info::Info;

/// Internal Rpc called in the Dht thread loop, useful to create your own actor setup.
#[derive(Debug)]
pub struct Actor {
    pub(crate) socket: KrpcSocket,
    core: Core,

    put_senders: HashMap<QueryId, Vec<Sender<Result<Id, PutError>>>>,
    get_senders: HashMap<QueryId, Vec<ResponseSender>>,
}

impl Actor {
    /// Create a new actor
    pub fn new(config: config::Config) -> Result<Self, std::io::Error> {
        let id = if let Some(ip) = config.public_ip {
            Id::from_ip(ip.into())
        } else {
            Id::random()
        };

        let socket = KrpcSocket::new(&config)?;
        let bootstrap = config
            .bootstrap
            .iter()
            .flat_map(|s| {
                s.to_socket_addrs().map(|addrs| {
                    addrs
                        .filter_map(|addr| match addr {
                            SocketAddr::V4(addr_v4) => Some(addr_v4),
                            SocketAddr::V6(_) => None,
                        })
                        .collect::<Box<[_]>>()
                })
            })
            .flatten()
            .collect();

        let address = socket.local_addr();
        info!(?address, "Mainline DHT started");

        Ok(Actor {
            socket,
            core: Core::new(id, bootstrap, config.server_mode, config.server_settings),

            put_senders: HashMap::new(),
            get_senders: HashMap::new(),
        })
    }

    // === Getters ===

    /// Returns the node's Id
    pub fn id(&self) -> &Id {
        self.core.routing_table.id()
    }

    /// Create a list of unique bootstrapping nodes from all our routing tables
    /// to use as `extra_bootstrap` in the next sessions.
    pub fn to_bootstrap(&self) -> Vec<String> {
        let mut set = HashSet::new();
        for s in self.core.routing_table.to_bootstrap() {
            set.insert(s);
        }
        for s in self.core.signed_peers_routing_table.to_bootstrap() {
            set.insert(s);
        }

        set.iter().cloned().collect()
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
    pub fn tick(&mut self) {
        self.periodic_node_maintenance();

        let new_query_response = self
            .socket
            .recv_from()
            .and_then(|(message, from)| self.handle_incoming_message(message, from));

        // Response for an ongoing GET query
        if let Some((id, response)) = new_query_response {
            if let Some(senders) = self.get_senders.get(&id) {
                for sender in senders {
                    send(sender, response.clone());
                }
            }
        }

        let mut done_put_queries = self.check_done_put_queries();

        for query in self.core.iterative_queries.values_mut() {
            query.visit_closest(&mut self.socket);
        }

        let done_iterative_queries = self.check_done_iterative_queries();

        self.start_put_queries(&done_iterative_queries, &mut done_put_queries);

        let should_ping_alleged_new_address = self
            .core
            .cleanup_done_queries(&done_iterative_queries, &done_put_queries);

        if let Some(address) = should_ping_alleged_new_address {
            self.ping(address);
        }

        // Cleanup done iterative queries
        for (id, closest_nodes) in done_iterative_queries {
            if let Some(senders) = self.get_senders.remove(&id) {
                for sender in senders {
                    match sender {
                        ResponseSender::ClosestNodes(sender) => {
                            let _ = sender.send(closest_nodes.clone());
                        }
                        // Query finished without finding a value;
                        // let a waiting [Dht::get_immutable] know it wasn't found.
                        ResponseSender::Immutable(sender) => {
                            let _ = sender.send(None);
                        }
                        _ => {}
                    }
                }
            }
        }

        self.send_put_results(&done_put_queries);
    }

    fn send_put_results(&mut self, done_put_queries: &[(QueryId, Result<Id, PutError>)]) {
        for (id, result) in done_put_queries {
            if let Some(senders) = self.put_senders.remove(id) {
                for sender in senders {
                    let _ = sender.send(result.clone());
                }
            }
        }
    }

    /// Remove finished put queries and notify their callers. Call this method
    /// before the conflict check in [`Actor::put`]. Then a finished put cannot
    /// block a new put.
    fn finish_done_put_queries(&mut self) {
        let done = self.check_done_put_queries();
        for (id, _) in &done {
            self.core.put_queries.remove(id);
        }
        self.send_put_results(&done);
    }

    /// Store a value in the closest nodes, optionally trigger a lookup query if
    /// the cached `closest_nodes` aren't fresh enough.
    pub fn put(
        &mut self,
        request: &PutRequestSpecific,
        extra_nodes: Option<Box<[Node]>>,
    ) -> Result<QueryId, PutError> {
        if let PutRequestSpecific::PutMutable(incoming) = request {
            self.finish_done_put_queries();
            match self.core.check_pending_put(incoming) {
                Ok(Some(id)) => return Ok(id),
                Ok(None) => {}
                Err(error) => return Err(error.into()),
            }
        }

        let mut query = PutQuery::new(request.clone(), extra_nodes);

        if let Some(closest_nodes) = self.core.get_cached_closest_nodes(request.target()) {
            query.start(&mut self.socket, &closest_nodes)?;
        } else {
            query.lookup_id = Some(self.get(GetRequestSpecific::from(request), None).0);
        }

        let id = self.core.allocate_query_id();
        self.core.put_queries.insert(id, query);

        Ok(id)
    }

    /// Send a message to closer and closer nodes until we can't find any more nodes.
    ///
    /// Queries take a few seconds to fully traverse the network. When a query
    /// finishes, it is removed from `self.iterative_queries`. Until then,
    /// identical requests share the lookup and receive the responses seen so
    /// far. Different requests use separate internal lookup ids.
    ///
    /// Effectively, we are caching responses and backing off the network for the duration it takes
    /// to traverse it.
    ///
    /// - `request` [`RequestTypeSpecific`], except [`RequestTypeSpecific::Ping`] and
    ///   [`RequestTypeSpecific::Put`] which will be ignored.
    /// - `extra_nodes` allows the query to visit specific nodes that the query
    ///   would not visit otherwise.
    pub fn get(
        &mut self,
        request: GetRequestSpecific,
        extra_nodes: Option<&[SocketAddrV4]>,
    ) -> (QueryId, Vec<Response>) {
        let target = request.target();

        let mut responses = vec![];

        if let Some(response_from_outgoing_request) = self.core.check_outgoing_put_request(&request)
        {
            responses.push(response_from_outgoing_request);
        }

        if let Some((id, responses_from_active_query)) = self.core.find_active_query(&request) {
            responses.extend_from_slice(responses_from_active_query);

            return (id, responses);
        }

        let (id, mut query, to_visit) = self.core.create_iterative_query(request, extra_nodes);

        let node_id = self.id();
        if target == *node_id {
            debug!(?node_id, "Bootstrapping the routing table");
        }

        for address in to_visit {
            query.visit(&mut self.socket, address);
        }

        self.core.iterative_queries.insert(id, query);

        (id, responses)
    }

    // === Private Methods ===

    fn periodic_node_maintenance(&mut self) {
        // Bootstrap if necessary
        if self.core.routing_table.is_empty() {
            self.populate();
        }

        // Every 15 minutes refresh the routing table.
        if self.core.should_refresh_table() {
            self.core.update_last_table_refresh();
            if !self.core.server_mode && !self.core.firewalled {
                info!(
                    "Adaptive mode: have been running long enough (not firewalled), \
                     switching to server mode"
                );

                self.set_server_mode(true);
            }
            self.populate();
        }

        if self.core.should_ping_table() {
            self.core.update_last_table_ping();
            let to_ping = self.core.check_nodes_to_ping_and_remove_stale_nodes();
            for address in to_ping {
                self.ping(address);
            }
        }
    }

    fn set_server_mode(&mut self, mode: bool) {
        self.socket.server_mode = mode;
        self.core.server_mode = mode;
    }

    fn handle_incoming_message(
        &mut self,
        message: Message,
        from: SocketAddrV4,
    ) -> Option<(QueryId, Response)> {
        match message.message_type {
            MessageType::Request(request_specific) => {
                let (response, should_repopulate_routing_tables) =
                    self.core
                        .handle_request(from, message.read_only, request_specific);

                match response {
                    Some(MessageType::Error(error)) => {
                        self.socket.error(from, message.transaction_id, error);
                    }
                    Some(MessageType::Response(response)) => {
                        self.socket.response(from, message.transaction_id, response);
                    }
                    _ => {}
                }

                if should_repopulate_routing_tables {
                    self.populate();
                }

                for address in self.core.take_pending_pings() {
                    self.ping(address);
                }

                None
            }
            _ => self.core.handle_response(from, message),
        }
    }

    fn check_done_put_queries(&self) -> Vec<(QueryId, Result<Id, PutError>)> {
        self.core
            .put_queries
            .iter()
            .filter_map(|(id, query)| match query.check(&self.socket) {
                Ok(done) => {
                    if done {
                        Some((*id, Ok(query.target)))
                    } else {
                        None
                    }
                }
                Err(error) => Some((*id, Err(error))),
            })
            .collect()
    }

    fn check_done_iterative_queries(&self) -> Vec<(QueryId, Box<[Node]>)> {
        self.core
            .iterative_queries
            .iter()
            .filter_map(|(id, query)| {
                let is_done = query.is_done(&self.socket);
                if is_done {
                    Some((
                        *id,
                        self.core.closest_nodes_from_done_iterative_query(query),
                    ))
                } else {
                    None
                }
            })
            .collect()
    }

    fn start_put_queries(
        &mut self,
        done_iterative_queries: &[(QueryId, Box<[Node]>)],
        done_put_queries: &mut Vec<(QueryId, Result<Id, PutError>)>,
    ) {
        for (lookup_id, closest_nodes) in done_iterative_queries {
            for (id, put_query) in &mut self.core.put_queries {
                if put_query.lookup_id != Some(*lookup_id) {
                    continue;
                }
                if let Err(error) = put_query.start(&mut self.socket, closest_nodes) {
                    done_put_queries.push((*id, Err(error)));
                }
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

pub(crate) fn run(config: Config, receiver: &Receiver<ActorMessage>) {
    match Actor::new(config) {
        Ok(mut actor) => {
            loop {
                match receiver.try_recv() {
                    Ok(actor_message) => match actor_message {
                        ActorMessage::Check(sender) => {
                            let _ = sender.send(Ok(()));
                        }
                        ActorMessage::Info(sender) => {
                            let _ = sender.send(actor.info());
                        }
                        ActorMessage::Put(request, sender, extra_nodes) => {
                            match actor.put(&request, extra_nodes) {
                                Ok(id) => {
                                    let senders = actor.put_senders.entry(id).or_insert(vec![]);

                                    senders.push(sender);
                                }
                                Err(error) => {
                                    let _ = sender.send(Err(error));
                                }
                            }
                        }
                        ActorMessage::Get(request, sender) => {
                            let (id, responses) = actor.get(request, None);
                            for response in responses {
                                send(&sender, response);
                            }

                            let senders = actor.get_senders.entry(id).or_insert(vec![]);

                            senders.push(sender);
                        }
                        ActorMessage::ToBootstrap(sender) => {
                            let _ = sender.send(actor.to_bootstrap());
                        }
                    },
                    Err(TryRecvError::Disconnected) => {
                        // Node was dropped, kill this thread.
                        tracing::debug!("dht::Dht's actor thread was shutdown after Drop.");
                        break;
                    }
                    Err(TryRecvError::Empty) => {
                        // No op
                    }
                }

                actor.tick();
            }
        }
        Err(err) => {
            if let Ok(ActorMessage::Check(sender)) = receiver.try_recv() {
                let _ = sender.send(Err(err));
            }
        }
    }
}

fn send(sender: &ResponseSender, response: Response) {
    match (sender, response) {
        (ResponseSender::Peers(s), Response::Peers(r)) => {
            let _ = s.send(r);
        }
        (ResponseSender::SignedPeers(s), Response::SignedPeers(r)) => {
            let _ = s.send(r);
        }
        (ResponseSender::Mutable(s), Response::Mutable(r)) => {
            let _ = s.send(r);
        }
        (ResponseSender::Immutable(s), Response::Immutable(r)) => {
            let _ = s.send(Some(r));
        }
        _ => {}
    }
}

#[derive(Debug)]
pub(crate) enum ActorMessage {
    Info(Sender<Info>),
    Put(
        PutRequestSpecific,
        Sender<Result<Id, PutError>>,
        Option<Box<[Node]>>,
    ),
    Get(GetRequestSpecific, ResponseSender),
    Check(Sender<Result<(), std::io::Error>>),
    ToBootstrap(Sender<Vec<String>>),
}

#[derive(Debug, Clone)]
pub(crate) enum ResponseSender {
    ClosestNodes(Sender<Box<[Node]>>),
    Peers(Sender<Vec<SocketAddrV4>>),
    SignedPeers(Sender<Vec<SignedAnnounce>>),
    Mutable(Sender<MutableItem>),
    Immutable(Sender<Option<Box<[u8]>>>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{GetPeersRequestArguments, GetValueRequestArguments};
    use crate::core::ConcurrencyError;

    fn actor() -> eyre::Result<Actor> {
        Ok(Actor::new(config::Config {
            port: Some(0),
            bootstrap: vec![],
            ..Default::default()
        })?)
    }

    #[test]
    fn lookups_share_ids_only_for_identical_requests() -> eyre::Result<()> {
        let mut actor = actor()?;
        let target = Id::from([9; 20]);
        let requests = [
            GetRequestSpecific::FindNode(FindNodeRequestArguments { target }),
            GetRequestSpecific::GetPeers(GetPeersRequestArguments { info_hash: target }),
            GetRequestSpecific::GetValue(GetValueRequestArguments {
                target,
                seq: None,
                salt: None,
            }),
            GetRequestSpecific::GetValue(GetValueRequestArguments {
                target,
                seq: Some(1),
                salt: None,
            }),
            GetRequestSpecific::GetValue(GetValueRequestArguments {
                target,
                seq: None,
                salt: Some(Box::from(&b"salt"[..])),
            }),
        ];

        let mut ids = HashSet::new();
        for request in requests {
            let (id, _) = actor.get(request.clone(), None);
            assert!(ids.insert(id), "different requests need different ids");
            assert_eq!(
                actor.get(request, None).0,
                id,
                "identical requests share their lookup"
            );
        }
        assert_eq!(actor.core.iterative_queries.len(), ids.len());
        Ok(())
    }

    fn mutable_request(value: &[u8], seq: i64, cas: Option<i64>) -> crate::PutRequestSpecific {
        let signer = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let item = crate::MutableItem::new(&signer, value, seq, None);
        crate::PutRequestSpecific::PutMutable(crate::common::PutMutableRequestArguments::from(
            item, cas,
        ))
    }

    #[test]
    fn pending_put_conflicts_map_to_put_errors() -> eyre::Result<()> {
        let mut actor = actor()?;
        let pending_request = mutable_request(b"a", 2, None);
        let pending_id = actor.core.allocate_query_id();
        actor
            .core
            .put_queries
            .insert(pending_id, PutQuery::new(pending_request.clone(), None));

        assert!(matches!(
            actor.put(&mutable_request(b"b", 2, None), None),
            Err(PutError::Concurrency(ConcurrencyError::ConflictRisk))
        ));
        assert_eq!(actor.put(&pending_request, None)?, pending_id);
        Ok(())
    }

    #[test]
    fn finished_put_does_not_block_a_new_put() -> eyre::Result<()> {
        use std::net::SocketAddrV4;
        use std::time::Duration;

        let mut actor = actor()?;
        let pending_request = mutable_request(b"a", 2, None);
        let pending_id = actor.core.allocate_query_id();
        let mut pending = PutQuery::new(pending_request, None);
        let node = Node::new_with_token(
            Id::random(),
            SocketAddrV4::new([127, 0, 0, 1].into(), 12345),
            vec![1].into(),
        );
        pending.start(&mut actor.socket, &[node])?;
        actor.core.put_queries.insert(pending_id, pending);

        let (sender, receiver) = flume::bounded(1);
        actor.put_senders.insert(pending_id, vec![sender]);

        std::thread::sleep(crate::actor::socket::MIN_REQUEST_TIMEOUT + Duration::from_millis(100));

        let new_id = actor.put(&mutable_request(b"b", 2, None), None)?;

        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1))?,
            Err(PutError::Query(crate::core::PutQueryError::Timeout))
        ));
        assert!(!actor.core.put_queries.contains_key(&pending_id));
        assert!(actor.core.put_queries.contains_key(&new_id));
        Ok(())
    }

    #[test]
    fn done_puts_deliver_their_own_results() -> eyre::Result<()> {
        let mut actor = actor()?;
        let (sender_a, receiver_a) = flume::bounded(1);
        let (sender_b, receiver_b) = flume::bounded(1);
        actor.put_senders.insert(1, vec![sender_a]);
        actor.put_senders.insert(2, vec![sender_b]);

        let target = Id::from([4; 20]);
        actor.send_put_results(&[
            (1, Err(PutError::Query(crate::core::PutQueryError::Timeout))),
            (2, Ok(target)),
        ]);

        assert!(matches!(
            receiver_a.recv()?,
            Err(PutError::Query(crate::core::PutQueryError::Timeout))
        ));
        let result = receiver_b.recv()?;
        assert!(matches!(result, Ok(id) if id == target));
        Ok(())
    }
}
