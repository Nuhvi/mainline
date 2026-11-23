use std::net::{Ipv4Addr, SocketAddrV4};

use tracing::{debug, trace};

use crate::common::{
    validate_immutable, GetImmutableResponseArguments, GetMutableResponseArguments,
    GetPeersResponseArguments, GetSignedPeersResponseArguments, Id, Message,
    NoMoreRecentValueResponseArguments, NoValuesResponseArguments, RequestTypeSpecific,
    ResponseSpecific, SignedAnnounce,
};
use crate::core::{supports_signed_peers, Response};
use crate::{MessageType, MutableItem, Node};

use super::{Core, QueryId};

impl Core {
    pub(crate) fn handle_response(
        &mut self,
        from: SocketAddrV4,
        message: Message,
    ) -> Option<(QueryId, Response)> {
        // If someone claims to be readonly, then let's not store anything even if they respond.
        if message.read_only {
            return None;
        }

        let Ok(transaction_id) = <[u8; 4]>::try_from(message.transaction_id.as_ref()) else {
            return None;
        };
        let transaction_id = u32::from_be_bytes(transaction_id);

        let author_id = message.get_author_id();
        let from_version = message.version;

        // If the response looks like a Ping response, check StoreQueries for the transaction_id.
        if let Some(query) = self
            .put_queries
            .values_mut()
            .find(|query| query.inflight(transaction_id))
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
        if let Some((query_id, query)) = self
            .iterative_queries
            .iter_mut()
            .find(|(_, query)| query.inflight(transaction_id))
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

            // `salt` is not on the wire. A mutable read carries it in the request.
            let salt = match &query.request.request_type {
                RequestTypeSpecific::GetValue(args) => args.salt.clone(),
                _ => None,
            };

            match message.message_type {
                MessageType::Response(ResponseSpecific::GetPeers(GetPeersResponseArguments {
                    values,
                    ..
                })) => {
                    let response = Response::Peers(values);
                    query.response(from, &response);

                    return Some((*query_id, response));
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

                        return Some((*query_id, response));
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

                        return Some((*query_id, response));
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

                    should_add_node = false;
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
                )) => match MutableItem::from_dht_message(query.target(), &k, v, seq, &sig, salt) {
                    Ok(item) => {
                        let min_seq = match &query.request.request_type {
                            RequestTypeSpecific::GetValue(args) => args.seq,
                            _ => None,
                        };
                        if min_seq.is_none_or(|seq| item.seq() > seq) {
                            let response = Response::Mutable(item);
                            query.response(from, &response);

                            return Some((*query_id, response));
                        }

                        debug!(
                            ?from,
                            ?responder_id,
                            ?from_version,
                            item_seq = item.seq(),
                            ?min_seq,
                            "Mutable record is not more recent than requested"
                        );
                    }
                    Err(error) => {
                        debug!(
                            ?error,
                            ?from,
                            ?responder_id,
                            ?from_version,
                            "Invalid mutable record"
                        );

                        should_add_node = false;
                    }
                },
                MessageType::Response(ResponseSpecific::NoMoreRecentValue(
                    NoMoreRecentValueResponseArguments {
                        seq, responder_id, ..
                    },
                )) => {
                    trace!(
                        target = ?query.target(),
                        ?salt,
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
                        target = ?query.target(),
                        ?salt,
                        ?from,
                        ?responder_id,
                        ?from_version,
                        "No values"
                    );
                }
                MessageType::Error(error) => {
                    debug!(?error, ?from_version, "Get query got error response");
                }
                // A Ping response adds the responder below. A FindNode
                // response adds its closer nodes above. Requests are handled
                // in the actor.
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

    use ed25519_dalek::SigningKey;

    use crate::actor::socket::KrpcSocket;
    use crate::common::{
        hash_immutable, FindNodeRequestArguments, FindNodeResponseArguments,
        GetValueRequestArguments,
    };
    use crate::core::iterative_query::GetRequestSpecific;
    use crate::ServerSettings;

    use super::*;

    #[test]
    fn internet_node_cannot_aim_requests_at_local_network() -> Result<()> {
        let mut core = Core::new(Id::random(), vec![], false, ServerSettings::default());
        let mut socket = KrpcSocket::client()?;
        let target = Id::random();
        let (id, mut query, _) = core.create_iterative_query(
            GetRequestSpecific::FindNode(FindNodeRequestArguments { target }),
            None,
        );

        let internet = SocketAddrV4::new([203, 0, 113, 1].into(), 6881);
        let tid = query.visit(&mut socket, internet);
        core.iterative_queries.insert(id, query);

        let public = Node::new(
            Id::random(),
            SocketAddrV4::new([198, 51, 100, 1].into(), 6881),
        );
        let local = Node::new(
            Id::random(),
            SocketAddrV4::new([192, 168, 1, 2].into(), 6881),
        );
        let response = Message {
            transaction_id: tid.to_be_bytes().into(),
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
            .get(&id)
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
    fn remote_mutable_reply_must_be_more_recent_than_requested() -> Result<()> {
        let signer = SigningKey::from_bytes(&[7; 32]);
        let item = MutableItem::new(&signer, b"value", 2, None);
        let target = *item.target();

        for (requested, accepted) in [
            (None, true),
            (Some(1), true),
            (Some(2), false),
            (Some(3), false),
        ] {
            let mut core = Core::new(Id::random(), vec![], false, ServerSettings::default());
            let (id, mut query, _) = core.create_iterative_query(
                GetRequestSpecific::GetValue(GetValueRequestArguments {
                    target,
                    seq: requested,
                    salt: None,
                }),
                None,
            );

            let mut socket = KrpcSocket::client()?;
            let from = SocketAddrV4::new([198, 51, 100, 1].into(), 6881);
            let tid = query.visit(&mut socket, from);
            core.iterative_queries.insert(id, query);

            let message = Message {
                transaction_id: tid.to_be_bytes().into(),
                version: None,
                requester_ip: None,
                read_only: false,
                message_type: MessageType::Response(ResponseSpecific::GetMutable(
                    GetMutableResponseArguments {
                        responder_id: Id::random(),
                        token: vec![1].into(),
                        nodes: None,
                        v: item.value().to_vec().into(),
                        k: *item.key(),
                        seq: item.seq(),
                        sig: *item.signature(),
                    },
                )),
            };
            let result = core.handle_response(from, message);

            assert_eq!(result.is_some(), accepted, "requested seq {requested:?}");
            let query = core
                .iterative_queries
                .get(&id)
                .ok_or_else(|| eyre!("the query must still exist"))?;
            assert_eq!(
                query.responses().len(),
                usize::from(accepted),
                "cached response at requested seq {requested:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn invalid_values_do_not_add_responder() -> Result<()> {
        let liar = SocketAddrV4::new([203, 0, 113, 1].into(), 6881);
        let responder_id = Id::random();

        // An immutable value that does not hash to the requested target.
        let mut core = Core::new(Id::random(), vec![], false, ServerSettings::default());
        let mut socket = KrpcSocket::client()?;
        let v: Box<[u8]> = b"value".to_vec().into();
        let target: Id = hash_immutable(&v).into();
        let (id, mut query, _) = core.create_iterative_query(
            GetRequestSpecific::GetValue(GetValueRequestArguments {
                target,
                seq: None,
                salt: None,
            }),
            None,
        );
        let tid = query.visit(&mut socket, liar);
        core.iterative_queries.insert(id, query);

        let response = Message {
            transaction_id: tid.to_be_bytes().into(),
            version: None,
            requester_ip: None,
            read_only: false,
            message_type: MessageType::Response(ResponseSpecific::GetImmutable(
                GetImmutableResponseArguments {
                    responder_id,
                    token: vec![1].into(),
                    nodes: None,
                    v: b"other".to_vec().into(),
                },
            )),
        };
        let _ = core.handle_response(liar, response);
        assert_eq!(
            core.routing_table.size(),
            0,
            "an invalid immutable value added its sender"
        );

        // A mutable item with an invalid signature.
        let mut core = Core::new(Id::random(), vec![], false, ServerSettings::default());
        let mut socket = KrpcSocket::client()?;
        let signer = SigningKey::from_bytes(&[7; 32]);
        let item = MutableItem::new(&signer, b"value", 1, None);
        let target = *item.target();
        let (id, mut query, _) = core.create_iterative_query(
            GetRequestSpecific::GetValue(GetValueRequestArguments {
                target,
                seq: None,
                salt: None,
            }),
            None,
        );
        let tid = query.visit(&mut socket, liar);
        core.iterative_queries.insert(id, query);

        let response = Message {
            transaction_id: tid.to_be_bytes().into(),
            version: None,
            requester_ip: None,
            read_only: false,
            message_type: MessageType::Response(ResponseSpecific::GetMutable(
                GetMutableResponseArguments {
                    responder_id,
                    token: vec![1].into(),
                    nodes: None,
                    v: item.value().to_vec().into(),
                    k: *item.key(),
                    seq: item.seq(),
                    sig: [0; 64],
                },
            )),
        };
        let _ = core.handle_response(liar, response);
        assert_eq!(
            core.routing_table.size(),
            0,
            "an invalid mutable item added its sender"
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
