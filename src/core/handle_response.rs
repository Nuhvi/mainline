use std::net::{Ipv4Addr, SocketAddrV4};

use tracing::{debug, trace};

use crate::{
    common::{
        validate_immutable, GetImmutableResponseArguments, GetMutableResponseArguments,
        GetPeersResponseArguments, GetSignedPeersResponseArguments, Id, Message,
        NoMoreRecentValueResponseArguments, NoValuesResponseArguments, RequestTypeSpecific,
        ResponseSpecific, SignedAnnounce,
    },
    core::{supports_signed_peers, Response},
    MessageType, MutableItem, Node,
};

use super::Core;

impl Core {
    pub(crate) fn handle_response(
        &mut self,
        from: SocketAddrV4,
        message: Message,
    ) -> Option<(Id, Response)> {
        // If someone claims to be readonly, then let's not store anything even if they respond.
        if message.read_only {
            return None;
        }

        let author_id = message.get_author_id();
        let from_version = message.version;

        // If the response looks like a Ping response, check StoreQueries for the transaction_id.
        if let Some(query) = self
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
            }

            self.add_responder(from, author_id, from_version);
            return None;
        }

        let mut should_add_node = true;

        // Get corresponding query for message.transaction_id
        if let Some(query) = self
            .iterative_queries
            .values_mut()
            .find(|query| query.inflight(message.transaction_id))
        {
            // KrpcSocket would not give us a response from the wrong address for the transaction_id
            query.responded(from);

            if let Some(nodes) = message.get_closer_nodes() {
                // A node on the Internet must not be able to point our requests at our local
                // network. Nodes in a local network may name each other, so that test networks
                // and DHTs inside a LAN keep working.
                let from_local_network = is_local_network(*from.ip());
                for node in nodes {
                    if from_local_network || !is_local_network(*node.address().ip()) {
                        query.add_candidate(node.clone());
                    }
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
                    query.response(from, &response);

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

                    for SignedAnnounce {
                        key,
                        timestamp,
                        signature,
                    } in peers
                    {
                        if let Ok(peer) =
                            SignedAnnounce::from_dht_response(&target, &key, timestamp, &signature)
                        {
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
                        query.response(from, &response);

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
                        query.response(from, &response);

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
                            query.response(from, &response);

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
                MessageType::Response(
                    ResponseSpecific::Ping(_) | ResponseSpecific::FindNode(_),
                )
                | MessageType::Request(_) => {}
            }
        }

        if should_add_node {
            self.add_responder(from, author_id, from_version);
        }

        None
    }

    /// Adds the responder to the routing tables if it has an author id.
    fn add_responder(
        &mut self,
        from: SocketAddrV4,
        author_id: Option<Id>,
        version: Option<[u8; 4]>,
    ) {
        if let Some(id) = author_id {
            self.routing_table.add(Node::new(id, from));

            if supports_signed_peers(version) {
                self.signed_peers_routing_table.add(Node::new(id, from));
            }
        }
    }
}

/// Returns whether `ip` is not a public Internet address.
fn is_local_network(ip: Ipv4Addr) -> bool {
    ip.is_private()
        || matches!(ip.octets(), [100, 64..=127, ..]) // shared address space, RFC 6598
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast()
}

#[cfg(test)]
mod tests {
    use eyre::{eyre, Result};

    use crate::{
        actor::socket::KrpcSocket,
        common::{FindNodeRequestArguments, FindNodeResponseArguments},
        core::iterative_query::GetRequestSpecific,
        ServerSettings,
    };

    use super::*;

    #[test]
    fn internet_node_cannot_aim_requests_at_local_network() -> Result<()> {
        let mut core = Core::new(Id::random(), vec![], false, ServerSettings::default());
        let mut socket = KrpcSocket::client()?;
        let target = Id::random();
        let (mut query, _) = core
            .create_iterative_query(
                GetRequestSpecific::FindNode(FindNodeRequestArguments { target }),
                None,
            )
            .ok_or_else(|| eyre!("a query for an unknown target must start"))?;

        let internet = SocketAddrV4::new([203, 0, 113, 1].into(), 6881);
        query.visit(&mut socket, internet);
        core.iterative_queries.insert(target, query);

        let public = Node::new(
            Id::random(),
            SocketAddrV4::new([198, 51, 100, 1].into(), 6881),
        );
        let local = Node::new(
            Id::random(),
            SocketAddrV4::new([192, 168, 1, 2].into(), 6881),
        );
        // A fresh socket sends its first request with transaction id 0.
        let response = Message {
            transaction_id: 0,
            version: None,
            requester_ip: None,
            read_only: false,
            message_type: MessageType::Response(ResponseSpecific::FindNode(
                FindNodeResponseArguments {
                    responder_id: Id::random(),
                    nodes: [public.clone(), local.clone()].into(),
                },
            )),
        };
        let _ = core.handle_response(internet, response);

        let closest = core
            .iterative_queries
            .get(&target)
            .ok_or_else(|| eyre!("the query must still exist"))?
            .closest()
            .nodes();
        assert!(closest.contains(&public), "the public node was dropped");
        assert!(
            !closest.contains(&local),
            "an Internet node pointed the lookup at the local network"
        );
        Ok(())
    }

    #[test]
    fn local_network_addresses() {
        for ip in [
            [10, 0, 0, 1],
            [172, 16, 0, 1],
            [192, 168, 1, 2],
            [100, 64, 0, 1],
            [100, 127, 255, 255],
            [127, 0, 0, 1],
            [169, 254, 0, 1],
            [0, 0, 0, 0],
            [224, 0, 0, 1],
            [255, 255, 255, 255],
        ] {
            assert!(is_local_network(ip.into()), "{ip:?}");
        }

        for ip in [
            [1, 1, 1, 1],
            [100, 63, 0, 1],
            [100, 128, 0, 1],
            [198, 51, 100, 1],
        ] {
            assert!(!is_local_network(ip.into()), "{ip:?}");
        }
    }
}
