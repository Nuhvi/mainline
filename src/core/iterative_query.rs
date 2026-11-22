//! Manage iterative queries and their corresponding request/response.

use std::collections::HashMap;
use std::net::SocketAddrV4;

use tracing::{debug, trace};

use crate::actor::socket::KrpcSocket;
use crate::common::{FindNodeRequestArguments, GetPeersRequestArguments, GetValueRequestArguments};
use crate::common::{Id, Node, RequestSpecific, RequestTypeSpecific, MAX_BUCKET_SIZE_K};
use crate::core::Response;
use crate::ClosestNodes;
use crate::PutRequestSpecific;

/// An iterative process of concurrently sending a request to the closest known nodes to
/// the target, updating the routing table with closer nodes discovered in the responses, and
/// repeating this process until no closer nodes (that aren't already queried) are found.
#[derive(Debug)]
pub(crate) struct IterativeQuery {
    pub request: RequestSpecific,
    closest: ClosestNodes,
    responders: ClosestNodes,
    inflight_requests: Vec<u32>,
    /// Nodes we sent a request to, with the transaction id until they respond.
    visited: HashMap<SocketAddrV4, Option<u32>>,
    responses: Vec<Response>,
    public_address_votes: HashMap<SocketAddrV4, u32>,
}

#[derive(Debug)]
pub enum GetRequestSpecific {
    FindNode(FindNodeRequestArguments),
    GetPeers(GetPeersRequestArguments),
    GetSignedPeers(GetPeersRequestArguments),
    GetValue(GetValueRequestArguments),
}

impl GetRequestSpecific {
    pub fn target(&self) -> Id {
        match self {
            GetRequestSpecific::FindNode(args) => args.target,
            GetRequestSpecific::GetPeers(args) | GetRequestSpecific::GetSignedPeers(args) => {
                args.info_hash
            }
            GetRequestSpecific::GetValue(args) => args.target,
        }
    }
}

impl From<&PutRequestSpecific> for GetRequestSpecific {
    fn from(request: &PutRequestSpecific) -> Self {
        match request {
            PutRequestSpecific::PutImmutable(args) => {
                GetRequestSpecific::GetValue(GetValueRequestArguments {
                    target: args.target,
                    seq: None,
                    salt: None,
                })
            }
            PutRequestSpecific::PutMutable(args) => {
                GetRequestSpecific::GetValue(GetValueRequestArguments {
                    target: args.target,
                    seq: None,
                    salt: args.salt.clone(),
                })
            }
            PutRequestSpecific::AnnouncePeer(args) => {
                GetRequestSpecific::GetPeers(GetPeersRequestArguments {
                    info_hash: args.info_hash,
                })
            }
            PutRequestSpecific::AnnounceSignedPeer(args) => {
                GetRequestSpecific::GetSignedPeers(GetPeersRequestArguments {
                    info_hash: args.info_hash,
                })
            }
        }
    }
}

impl IterativeQuery {
    pub(crate) fn new(requester_id: Id, target: Id, request: GetRequestSpecific) -> Self {
        let request_type = match request {
            GetRequestSpecific::FindNode(s) => RequestTypeSpecific::FindNode(s),
            GetRequestSpecific::GetPeers(s) => RequestTypeSpecific::GetPeers(s),
            GetRequestSpecific::GetSignedPeers(s) => RequestTypeSpecific::GetSignedPeers(s),
            GetRequestSpecific::GetValue(s) => RequestTypeSpecific::GetValue(s),
        };

        trace!(?target, ?request_type, "New Query");

        Self {
            request: RequestSpecific {
                requester_id,
                request_type,
            },

            closest: ClosestNodes::new(target),
            responders: ClosestNodes::new(target),

            inflight_requests: Vec::new(),
            visited: HashMap::new(),

            responses: Vec::new(),

            public_address_votes: HashMap::new(),
        }
    }

    // === Getters ===

    pub(crate) fn target(&self) -> Id {
        self.responders.target()
    }

    /// Closest nodes according to other nodes.
    pub(crate) fn closest(&self) -> &ClosestNodes {
        &self.closest
    }

    /// Return the closest responding nodes after the query is done.
    pub(crate) fn responders(&self) -> &ClosestNodes {
        &self.responders
    }

    pub(crate) fn responses(&self) -> &[Response] {
        &self.responses
    }

    pub(crate) fn best_address(&self) -> Option<SocketAddrV4> {
        let mut max = 0_u32;
        let mut best_addr = None;

        for (addr, count) in &self.public_address_votes {
            if *count > max {
                max = *count;
                best_addr = Some(*addr);
            }
        }

        best_addr
    }

    // === Public Methods ===

    /// Add a candidate node to query on next tick if it is among the closest nodes.
    pub(crate) fn add_candidate(&mut self, node: Node) {
        // ready for a ipv6 routing table?
        self.closest.add(node);
    }

    /// Add a vote for this node's address.
    pub(crate) fn add_address_vote(&mut self, address: SocketAddrV4) {
        self.public_address_votes
            .entry(address)
            .and_modify(|counter| *counter += 1)
            .or_insert(1);
    }

    /// Visit explicitly given addresses, add them to the visited set, and return the
    /// transaction id.
    /// only used from the Rpc when calling bootstrapping nodes.
    pub(crate) fn visit(&mut self, socket: &mut KrpcSocket, address: SocketAddrV4) -> u32 {
        let tid = socket.request(address, self.request.clone());
        self.inflight_requests.push(tid);
        self.visited.insert(address, Some(tid));
        tid
    }

    /// Return true if a response (by `transaction_id`) is expected by this query.
    pub(crate) fn inflight(&self, tid: u32) -> bool {
        self.inflight_requests.contains(&tid)
    }

    /// Record that the node at `address` responded to this query.
    pub(crate) fn responded(&mut self, address: SocketAddrV4) {
        if let Some(tid) = self.visited.get_mut(&address) {
            *tid = None;
        }
    }

    /// Add a node that responded with a token as a probable storage node.
    pub(crate) fn add_responding_node(&mut self, node: Node) {
        self.responders.add(node);
    }

    /// Store received response.
    pub(crate) fn response(&mut self, from: SocketAddrV4, response: &Response) {
        let target = self.target();

        trace!(?target, ?response, ?from, "Query got response");

        self.responses.push(response.clone());
    }

    pub(crate) fn visit_closest(&mut self, socket: &mut KrpcSocket) {
        let to_visit = self.next_candidates(socket);

        for address in to_visit {
            self.visit(socket, address);
        }
    }

    /// Returns the closest nodes for a new query.
    ///
    /// The query is new, so no node is visited yet.
    pub(crate) fn initial_candidates(&self) -> Vec<SocketAddrV4> {
        self.closest
            .nodes()
            .iter()
            .take(MAX_BUCKET_SIZE_K)
            .map(Node::address)
            .collect()
    }

    /// Returns the closest nodes that the query can visit now.
    ///
    /// A node whose request timed out frees its place among the closest nodes.
    fn next_candidates(&self, socket: &KrpcSocket) -> Vec<SocketAddrV4> {
        self.closest
            .nodes()
            .iter()
            // A node that timed out without a response loses its slot.
            .filter(|node| !self.timed_out(&node.address(), socket))
            .take(MAX_BUCKET_SIZE_K)
            // A pending or a responded node keeps its slot, but needs no request.
            .filter(|node| !self.visited.contains_key(&node.address()))
            .map(Node::address)
            .collect()
    }

    /// Returns true if the node was asked, did not respond,
    /// and its request is no longer inflight.
    fn timed_out(&self, address: &SocketAddrV4, socket: &KrpcSocket) -> bool {
        matches!(self.visited.get(address), Some(Some(tid)) if !socket.inflight(*tid))
    }

    /// Returns true if it is done.
    pub(crate) fn is_done(&self, socket: &KrpcSocket) -> bool {
        // If no more inflight_requests are inflight in the socket (not timed out),
        // then the query is done.
        let done = !self
            .inflight_requests
            .iter()
            .any(|tid| socket.inflight(*tid));

        if done {
            debug!(id=?self.target(), closest = ?self.closest.len(), visited = ?self.visited.len(), responders = ?self.responders.len(), "Done query");
        }

        done
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use eyre::{eyre, Result};

    use crate::actor::socket::MIN_REQUEST_TIMEOUT;

    use super::*;

    #[test]
    fn unresponsive_nodes_do_not_crowd_out_further_nodes() -> Result<()> {
        let mut socket = KrpcSocket::client()?;
        let target = Id::random();
        let mut query = IterativeQuery::new(
            Id::random(),
            target,
            GetRequestSpecific::GetValue(GetValueRequestArguments {
                target,
                seq: None,
                salt: None,
            }),
        );

        for distance in 1..=MAX_BUCKET_SIZE_K as u8 {
            let mut id = *target.as_bytes();
            let octet = id.get_mut(19).ok_or_else(|| eyre!("an id is 20 bytes"))?;
            *octet ^= distance;
            let address = SocketAddrV4::new([127, 66, 0, distance].into(), 1);
            query.add_candidate(Node::new(id.into(), address));
        }

        let mut id = *target.as_bytes();
        let octet = id.get_mut(0).ok_or_else(|| eyre!("an id is 20 bytes"))?;
        *octet ^= 1;
        let further = SocketAddrV4::new([127, 0, 0, 1].into(), 1);
        query.add_candidate(Node::new(id.into(), further));

        query.visit_closest(&mut socket);
        while !query.is_done(&socket) {
            thread::sleep(Duration::from_millis(50));
        }
        query.visit_closest(&mut socket);

        assert!(
            query.visited.contains_key(&further),
            "the farther node was never asked"
        );
        Ok(())
    }

    #[test]
    fn only_nodes_that_timed_out_without_responding_lose_their_place() -> Result<()> {
        let mut socket = KrpcSocket::client()?;
        let target = Id::random();
        let mut query = IterativeQuery::new(
            Id::random(),
            target,
            GetRequestSpecific::GetValue(GetValueRequestArguments {
                target,
                seq: None,
                salt: None,
            }),
        );
        let mut close = vec![];
        for i in 1..=MAX_BUCKET_SIZE_K as u8 {
            let mut id = *target.as_bytes();
            let octet = id.get_mut(19).ok_or_else(|| eyre!("an id is 20 bytes"))?;
            *octet ^= i;
            let address = SocketAddrV4::new([127, 66, 0, i].into(), 1);
            query.add_candidate(Node::new(id.into(), address));
            query.visit(&mut socket, address);
            close.push(address);
        }

        let mut id = *target.as_bytes();
        let octet = id.get_mut(0).ok_or_else(|| eyre!("an id is 20 bytes"))?;
        *octet ^= 1;
        let further = SocketAddrV4::new([127, 0, 0, 1].into(), 1);
        query.add_candidate(Node::new(id.into(), further));

        // Requests still inflight: the close nodes keep their places.
        assert!(query.next_candidates(&socket).is_empty());
        // Requests timed out without a response: the further node is next.
        thread::sleep(MIN_REQUEST_TIMEOUT + Duration::from_millis(100));
        assert_eq!(query.next_candidates(&socket), vec![further]);
        // Responses give the close nodes their places back, so the query is done.
        for address in close {
            query.responded(address);
        }
        assert!(query.next_candidates(&socket).is_empty());
        Ok(())
    }
}
