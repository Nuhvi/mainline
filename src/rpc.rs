//! K-RPC implementation.

pub(crate) mod config;
mod info;
pub(crate) mod socket;

use std::collections::HashSet;
use std::net::{SocketAddr, SocketAddrV4, ToSocketAddrs};

use tracing::{debug, info};

use crate::common::{
    FindNodeRequestArguments, Id, Message, MessageType, Node, PutRequestSpecific, RequestSpecific,
    RequestTypeSpecific,
};
use crate::core::{
    iterative_query::GetRequestSpecific, put_query::PutQuery, Core, PutError, Response,
};

use socket::KrpcSocket;

pub use info::Info;

#[derive(Debug)]
/// Internal Rpc called in the Dht thread loop, useful to create your own actor setup.
pub struct Rpc {
    pub(crate) socket: KrpcSocket,
    core: Core,
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
        let bootstrap = config
            .bootstrap
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
            .collect();

        Ok(Rpc {
            socket,
            core: Core::new(id, bootstrap, config.server_mode, config.server_settings),
        })
    }

    // === Getters ===

    /// Returns the node's Id
    pub fn id(&self) -> &Id {
        self.core.routing_table.id()
    }

    /// Create a list of unique bootstrapping nodes from all our
    /// routing table to use as `extra_bootsrtap` in next sessions.
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
    pub fn tick(&mut self) -> RpcTickReport {
        self.periodic_node_maintaenance();

        let new_query_response = self
            .socket
            .recv_from()
            .and_then(|(message, from)| self.handle_incoming_message(message, from));

        let mut done_put_queries = self.check_done_put_queries();

        for (_, query) in self.core.iterative_queries.iter_mut() {
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

        RpcTickReport {
            done_get_queries: done_iterative_queries,
            done_put_queries,
            new_query_response,
        }
    }

    /// Store a value in the closest nodes, optionally trigger a lookup query if
    /// the cached closest_nodes aren't fresh enough.
    pub fn put(
        &mut self,
        request: PutRequestSpecific,
        extra_nodes: Option<Box<[Node]>>,
    ) -> Result<(), PutError> {
        self.core.check_concurrency_errors(&request)?;

        let mut query = PutQuery::new(request.clone(), extra_nodes);

        let target = request.target();
        if let Some(closest_nodes) = self.core.get_cached_closest_nodes(target) {
            query.start(&mut self.socket, &closest_nodes)?
        } else {
            self.get(GetRequestSpecific::from(&request), None);
        };

        self.core.put_queries.insert(*target, query);

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
    ) -> Vec<Response> {
        let target = request.target();

        let node_id = self.id();
        if target == *node_id {
            debug!(?node_id, "Bootstrapping the routing table");
        }

        let mut responses = vec![];

        if let Some(response_from_outgoing_request) = self.core.check_outgoing_put_request(&target)
        {
            responses.push(response_from_outgoing_request);
        }

        if let Some(responses_from_active_query) =
            self.core.check_responses_from_active_query(&target)
        {
            responses.extend_from_slice(responses_from_active_query);

            // Terminate, no need to create another query.
            return responses;
        };

        if let Some((mut query, to_visit)) = self.core.create_iterative_query(request, extra_nodes)
        {
            for address in to_visit {
                query.visit(&mut self.socket, address);
            }

            self.core.iterative_queries.insert(target, query);
        }

        responses
    }

    // === Private Methods ===

    fn periodic_node_maintaenance(&mut self) {
        // Bootstrap if necessary
        if self.core.routing_table.is_empty() {
            self.populate();
        }

        // Every 15 minutes refresh the routing table.
        if self.core.should_refresh_table() {
            self.core.update_last_table_refresh();
            if !self.core.server_mode && !self.core.firewalled {
                info!("Adaptive mode: have been running long enough (not firewalled), switching to server mode");

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
            _ => self.core.handle_response(from, message),
        }
    }

    fn check_done_put_queries(&self) -> Vec<(Id, Option<PutError>)> {
        self.core
            .put_queries
            .iter()
            .filter_map(|(id, query)| match query.check(&self.socket) {
                Ok(done) => {
                    if done {
                        Some((*id, None))
                    } else {
                        None
                    }
                }
                Err(error) => Some((*id, Some(error))),
            })
            .collect()
    }

    fn check_done_iterative_queries(&self) -> Vec<(Id, Box<[Node]>)> {
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
        done_iterative_queries: &[(Id, Box<[Node]>)],
        done_put_queries: &mut Vec<(Id, Option<PutError>)>,
    ) {
        for (id, _) in done_iterative_queries {
            if let Some(put_query) = self.core.put_queries.get_mut(id) {
                if let Err(error) = put_query.start(
                    &mut self.socket,
                    done_iterative_queries
                        .iter()
                        .find(|(this_id, _)| this_id == id)
                        .map(|(_, closest_nodes)| closest_nodes)
                        .expect("done_iterative_queries"),
                ) {
                    done_put_queries.push((*id, Some(error)))
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
