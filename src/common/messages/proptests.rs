//! Property tests for the bencode depth guard and the message codec.
//!
//! The depth tests check that:
//! - The scanner accepts every well-formed bencode value.
//! - The scanner counts the nesting depth exactly.
//! - Decoding arbitrary and mutated packets does not panic and never
//!   reports `BencodeTooDeep` for a packet that is within the limit.
//!
//! The codec tests check that:
//! - Every message round-trips through the wire format.
//! - A decoded packet re-encodes to a packet with the same wire meaning.

use std::net::{Ipv4Addr, SocketAddrV4};

use proptest::prelude::*;

use super::*;

// ==== Depth guard ====

/// A well-formed bencode value and its nesting depth.
#[derive(Clone, Debug)]
struct Nested {
    bytes: Vec<u8>,
    depth: usize,
}

/// A byte string with a random payload.
///
/// The payload can contain `d`, `l`, `e` and `i`. The scanner must skip the
/// payload and must not count these bytes as structure.
fn string_leaf() -> impl Strategy<Value = Nested> {
    prop::collection::vec(any::<u8>(), 0..=32).prop_map(|payload| {
        let mut bytes = format!("{}:", payload.len()).into_bytes();
        bytes.extend_from_slice(&payload);
        Nested { bytes, depth: 0 }
    })
}

/// An integer.
fn int_leaf() -> impl Strategy<Value = Nested> {
    any::<i32>().prop_map(|number| Nested {
        bytes: format!("i{number}e").into_bytes(),
        depth: 0,
    })
}

/// A well-formed bencode value with at most `depth` nested containers.
fn nested_value(depth: usize) -> BoxedStrategy<Nested> {
    let leaf = prop_oneof![string_leaf(), int_leaf()].boxed();

    if depth == 0 {
        return leaf;
    }

    let list = prop::collection::vec(nested_value(depth - 1), 0..=3)
        .prop_map(|children| {
            let child_depth = children.iter().map(|child| child.depth).max().unwrap_or(0);
            let mut bytes = vec![b'l'];
            for child in &children {
                bytes.extend_from_slice(&child.bytes);
            }
            bytes.push(b'e');
            Nested {
                bytes,
                depth: child_depth + 1,
            }
        })
        .boxed();

    let dict = prop::collection::vec((string_leaf(), nested_value(depth - 1)), 0..=3)
        .prop_map(|pairs| {
            let child_depth = pairs
                .iter()
                .map(|(_, value)| value.depth)
                .max()
                .unwrap_or(0);
            let mut bytes = vec![b'd'];
            for (key, value) in &pairs {
                bytes.extend_from_slice(&key.bytes);
                bytes.extend_from_slice(&value.bytes);
            }
            bytes.push(b'e');
            Nested {
                bytes,
                depth: child_depth + 1,
            }
        })
        .boxed();

    prop_oneof![leaf, list, dict].boxed()
}

// ==== Message strategies ====

/// A random 20-byte node id.
fn id() -> impl Strategy<Value = Id> {
    any::<[u8; 20]>().prop_map(|bytes| match Id::from_bytes(bytes) {
        Ok(id) => id,
        // The array is always 20 bytes, so this arm is unreachable.
        Err(_) => Id::random(),
    })
}

/// A random IPv4 socket address.
fn socket_addr() -> impl Strategy<Value = SocketAddrV4> {
    (any::<[u8; 4]>(), any::<u16>())
        .prop_map(|(octets, port)| SocketAddrV4::new(Ipv4Addr::from(octets), port))
}

/// A random node.
fn node() -> impl Strategy<Value = Node> {
    (id(), socket_addr()).prop_map(|(id, address)| Node::new(id, address))
}

/// A random short node list.
fn nodes() -> impl Strategy<Value = Box<[Node]>> {
    prop::collection::vec(node(), 0..=4).prop_map(Vec::into_boxed_slice)
}

/// An optional node list.
fn maybe_nodes() -> impl Strategy<Value = Option<Box<[Node]>>> {
    prop::option::of(nodes())
}

/// A random byte blob with at most `max` bytes.
fn blob(max: usize) -> impl Strategy<Value = Box<[u8]>> {
    prop::collection::vec(any::<u8>(), 0..=max).prop_map(Vec::into_boxed_slice)
}

/// A random signed announcement. The signature is not verified when the
/// message is decoded, so random bytes are correct here.
fn signed_announce() -> impl Strategy<Value = SignedAnnounce> {
    (any::<[u8; 32]>(), any::<u64>(), any::<[u8; 64]>()).prop_map(|(key, timestamp, signature)| {
        SignedAnnounce {
            key,
            timestamp,
            signature,
        }
    })
}

/// A random short string.
fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..=24).prop_map(String::from_iter)
}

fn ping_request() -> BoxedStrategy<MessageType> {
    id().prop_map(|requester_id| {
        MessageType::Request(RequestSpecific {
            requester_id,
            request_type: RequestTypeSpecific::Ping,
        })
    })
    .boxed()
}

fn find_node_request() -> BoxedStrategy<MessageType> {
    (id(), id())
        .prop_map(|(requester_id, target)| {
            MessageType::Request(RequestSpecific {
                requester_id,
                request_type: RequestTypeSpecific::FindNode(FindNodeRequestArguments { target }),
            })
        })
        .boxed()
}

fn get_peers_request() -> BoxedStrategy<MessageType> {
    (id(), id())
        .prop_map(|(requester_id, info_hash)| {
            MessageType::Request(RequestSpecific {
                requester_id,
                request_type: RequestTypeSpecific::GetPeers(GetPeersRequestArguments { info_hash }),
            })
        })
        .boxed()
}

fn get_signed_peers_request() -> BoxedStrategy<MessageType> {
    (id(), id())
        .prop_map(|(requester_id, info_hash)| {
            MessageType::Request(RequestSpecific {
                requester_id,
                request_type: RequestTypeSpecific::GetSignedPeers(GetPeersRequestArguments {
                    info_hash,
                }),
            })
        })
        .boxed()
}

fn get_value_request() -> BoxedStrategy<MessageType> {
    (id(), id(), prop::option::of(any::<i64>()))
        .prop_map(|(requester_id, target, seq)| {
            MessageType::Request(RequestSpecific {
                requester_id,
                request_type: RequestTypeSpecific::GetValue(GetValueRequestArguments {
                    target,
                    seq,
                    // `salt` is documented as not encoded on the wire.
                    salt: None,
                }),
            })
        })
        .boxed()
}

fn announce_peer_request() -> BoxedStrategy<MessageType> {
    (
        id(),
        blob(20),
        id(),
        any::<u16>(),
        prop::option::of(any::<bool>()),
    )
        .prop_map(|(requester_id, token, info_hash, port, implied_port)| {
            MessageType::Request(RequestSpecific {
                requester_id,
                request_type: RequestTypeSpecific::Put(PutRequest {
                    token,
                    put_request_type: PutRequestSpecific::AnnouncePeer(
                        AnnouncePeerRequestArguments {
                            info_hash,
                            port,
                            implied_port,
                        },
                    ),
                }),
            })
        })
        .boxed()
}

fn announce_signed_peer_request() -> BoxedStrategy<MessageType> {
    (
        id(),
        blob(20),
        id(),
        any::<u64>(),
        any::<[u8; 32]>(),
        any::<[u8; 64]>(),
    )
        .prop_map(|(requester_id, token, info_hash, t, k, sig)| {
            MessageType::Request(RequestSpecific {
                requester_id,
                request_type: RequestTypeSpecific::Put(PutRequest {
                    token,
                    put_request_type: PutRequestSpecific::AnnounceSignedPeer(
                        AnnounceSignedPeerRequestArguments {
                            info_hash,
                            t,
                            k,
                            sig,
                        },
                    ),
                }),
            })
        })
        .boxed()
}

fn put_immutable_request() -> BoxedStrategy<MessageType> {
    (id(), blob(20), id(), blob(64))
        .prop_map(|(requester_id, token, target, v)| {
            MessageType::Request(RequestSpecific {
                requester_id,
                request_type: RequestTypeSpecific::Put(PutRequest {
                    token,
                    put_request_type: PutRequestSpecific::PutImmutable(
                        PutImmutableRequestArguments { target, v },
                    ),
                }),
            })
        })
        .boxed()
}

fn put_mutable_request() -> BoxedStrategy<MessageType> {
    (
        id(),
        blob(20),
        id(),
        blob(64),
        any::<[u8; 32]>(),
        any::<i64>(),
        any::<[u8; 64]>(),
        prop::option::of(blob(16)),
        prop::option::of(any::<i64>()),
    )
        .prop_map(|(requester_id, token, target, v, k, seq, sig, salt, cas)| {
            MessageType::Request(RequestSpecific {
                requester_id,
                request_type: RequestTypeSpecific::Put(PutRequest {
                    token,
                    put_request_type: PutRequestSpecific::PutMutable(PutMutableRequestArguments {
                        target,
                        v,
                        k,
                        seq,
                        sig,
                        salt,
                        cas,
                    }),
                }),
            })
        })
        .boxed()
}

fn ping_response() -> BoxedStrategy<MessageType> {
    id().prop_map(|responder_id| {
        MessageType::Response(ResponseSpecific::Ping(PingResponseArguments {
            responder_id,
        }))
    })
    .boxed()
}

fn find_node_response() -> BoxedStrategy<MessageType> {
    (id(), nodes())
        .prop_map(|(responder_id, nodes)| {
            MessageType::Response(ResponseSpecific::FindNode(FindNodeResponseArguments {
                responder_id,
                nodes,
            }))
        })
        .boxed()
}

fn get_peers_response() -> BoxedStrategy<MessageType> {
    (
        id(),
        blob(20),
        prop::collection::vec(socket_addr(), 0..=4),
        maybe_nodes(),
    )
        .prop_map(|(responder_id, token, values, nodes)| {
            MessageType::Response(ResponseSpecific::GetPeers(GetPeersResponseArguments {
                responder_id,
                token,
                values,
                nodes,
            }))
        })
        .boxed()
}

fn get_signed_peers_response() -> BoxedStrategy<MessageType> {
    (
        id(),
        blob(20),
        prop::collection::vec(signed_announce(), 0..=4),
        maybe_nodes(),
    )
        .prop_map(|(responder_id, token, peers, nodes)| {
            MessageType::Response(ResponseSpecific::GetSignedPeers(
                GetSignedPeersResponseArguments {
                    responder_id,
                    token,
                    peers,
                    nodes,
                },
            ))
        })
        .boxed()
}

fn no_values_response() -> BoxedStrategy<MessageType> {
    (id(), blob(20), maybe_nodes())
        .prop_map(|(responder_id, token, nodes)| {
            MessageType::Response(ResponseSpecific::NoValues(NoValuesResponseArguments {
                responder_id,
                token,
                nodes,
            }))
        })
        .boxed()
}

fn get_immutable_response() -> BoxedStrategy<MessageType> {
    (id(), blob(20), maybe_nodes(), blob(64))
        .prop_map(|(responder_id, token, nodes, v)| {
            MessageType::Response(ResponseSpecific::GetImmutable(
                GetImmutableResponseArguments {
                    responder_id,
                    token,
                    nodes,
                    v,
                },
            ))
        })
        .boxed()
}

fn get_mutable_response() -> BoxedStrategy<MessageType> {
    (
        id(),
        blob(20),
        maybe_nodes(),
        blob(64),
        any::<[u8; 32]>(),
        any::<i64>(),
        any::<[u8; 64]>(),
    )
        .prop_map(|(responder_id, token, nodes, v, k, seq, sig)| {
            MessageType::Response(ResponseSpecific::GetMutable(GetMutableResponseArguments {
                responder_id,
                token,
                nodes,
                v,
                k,
                seq,
                sig,
            }))
        })
        .boxed()
}

fn no_more_recent_value_response() -> BoxedStrategy<MessageType> {
    (id(), blob(20), maybe_nodes(), any::<i64>())
        .prop_map(|(responder_id, token, nodes, seq)| {
            MessageType::Response(ResponseSpecific::NoMoreRecentValue(
                NoMoreRecentValueResponseArguments {
                    responder_id,
                    token,
                    nodes,
                    seq,
                },
            ))
        })
        .boxed()
}

fn error_response() -> BoxedStrategy<MessageType> {
    (any::<i32>(), text())
        .prop_map(|(code, description)| MessageType::Error(ErrorSpecific { code, description }))
        .boxed()
}

/// A random message of any kind.
fn any_message() -> BoxedStrategy<Message> {
    let message_type = prop_oneof![
        prop_oneof![
            ping_request(),
            find_node_request(),
            get_peers_request(),
            get_signed_peers_request(),
            get_value_request(),
        ],
        prop_oneof![
            announce_peer_request(),
            announce_signed_peer_request(),
            put_immutable_request(),
            put_mutable_request(),
        ],
        prop_oneof![
            ping_response(),
            find_node_response(),
            get_peers_response(),
            get_signed_peers_response(),
            no_values_response(),
        ],
        prop_oneof![
            get_immutable_response(),
            get_mutable_response(),
            no_more_recent_value_response(),
            error_response(),
        ],
    ]
    .boxed();

    (
        prop::collection::vec(any::<u8>(), 0..=4).prop_map(Vec::into_boxed_slice),
        prop::option::of(any::<[u8; 4]>()),
        prop::option::of(socket_addr()),
        any::<bool>(),
        message_type,
    )
        .prop_map(
            |(transaction_id, version, requester_ip, read_only, message_type)| Message {
                transaction_id,
                version,
                requester_ip,
                read_only,
                message_type,
            },
        )
        .boxed()
}

// ==== Wire comparison ====

/// Compares two node lists by the data that is on the wire.
///
/// `Node` also carries a `last_seen` timestamp. That timestamp is not part
/// of the wire format, so two decoded nodes never match by `PartialEq`.
fn nodes_eq(a: &[Node], b: &[Node]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(a, b)| a.id() == b.id() && a.address() == b.address())
}

fn maybe_nodes_eq(a: &Option<Box<[Node]>>, b: &Option<Box<[Node]>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => nodes_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

fn put_eq(a: &PutRequestSpecific, b: &PutRequestSpecific) -> bool {
    match (a, b) {
        (PutRequestSpecific::AnnouncePeer(a), PutRequestSpecific::AnnouncePeer(b)) => {
            a.info_hash == b.info_hash
                && a.port == b.port
                // `None` and `Some(false)` are the same wire value.
                && a.implied_port.unwrap_or(false) == b.implied_port.unwrap_or(false)
        }
        (PutRequestSpecific::AnnounceSignedPeer(a), PutRequestSpecific::AnnounceSignedPeer(b)) => {
            a == b
        }
        (PutRequestSpecific::PutImmutable(a), PutRequestSpecific::PutImmutable(b)) => a == b,
        (PutRequestSpecific::PutMutable(a), PutRequestSpecific::PutMutable(b)) => a == b,
        _ => false,
    }
}

fn request_eq(a: &RequestSpecific, b: &RequestSpecific) -> bool {
    a.requester_id == b.requester_id
        && match (&a.request_type, &b.request_type) {
            (RequestTypeSpecific::Ping, RequestTypeSpecific::Ping) => true,
            (RequestTypeSpecific::FindNode(a), RequestTypeSpecific::FindNode(b)) => a == b,
            (RequestTypeSpecific::GetPeers(a), RequestTypeSpecific::GetPeers(b)) => a == b,
            (RequestTypeSpecific::GetSignedPeers(a), RequestTypeSpecific::GetSignedPeers(b)) => {
                a == b
            }
            (RequestTypeSpecific::GetValue(a), RequestTypeSpecific::GetValue(b)) => {
                // `salt` is not part of the wire format.
                a.target == b.target && a.seq == b.seq
            }
            (RequestTypeSpecific::Put(a), RequestTypeSpecific::Put(b)) => {
                a.token == b.token && put_eq(&a.put_request_type, &b.put_request_type)
            }
            _ => false,
        }
}

fn response_eq(a: &ResponseSpecific, b: &ResponseSpecific) -> bool {
    match (a, b) {
        (ResponseSpecific::Ping(a), ResponseSpecific::Ping(b)) => a == b,
        (ResponseSpecific::FindNode(a), ResponseSpecific::FindNode(b)) => {
            a.responder_id == b.responder_id && nodes_eq(&a.nodes, &b.nodes)
        }
        (ResponseSpecific::GetPeers(a), ResponseSpecific::GetPeers(b)) => {
            a.responder_id == b.responder_id
                && a.token == b.token
                && a.values == b.values
                && maybe_nodes_eq(&a.nodes, &b.nodes)
        }
        (ResponseSpecific::GetSignedPeers(a), ResponseSpecific::GetSignedPeers(b)) => {
            a.responder_id == b.responder_id
                && a.token == b.token
                && a.peers == b.peers
                && maybe_nodes_eq(&a.nodes, &b.nodes)
        }
        (ResponseSpecific::NoValues(a), ResponseSpecific::NoValues(b)) => {
            a.responder_id == b.responder_id
                && a.token == b.token
                && maybe_nodes_eq(&a.nodes, &b.nodes)
        }
        (ResponseSpecific::GetImmutable(a), ResponseSpecific::GetImmutable(b)) => {
            a.responder_id == b.responder_id
                && a.token == b.token
                && a.v == b.v
                && maybe_nodes_eq(&a.nodes, &b.nodes)
        }
        (ResponseSpecific::GetMutable(a), ResponseSpecific::GetMutable(b)) => {
            a.responder_id == b.responder_id
                && a.token == b.token
                && a.v == b.v
                && a.k == b.k
                && a.seq == b.seq
                && a.sig == b.sig
                && maybe_nodes_eq(&a.nodes, &b.nodes)
        }
        (ResponseSpecific::NoMoreRecentValue(a), ResponseSpecific::NoMoreRecentValue(b)) => {
            a.responder_id == b.responder_id
                && a.token == b.token
                && a.seq == b.seq
                && maybe_nodes_eq(&a.nodes, &b.nodes)
        }
        _ => false,
    }
}

/// Compares two messages by their wire meaning.
fn wire_eq(a: &Message, b: &Message) -> bool {
    a.transaction_id == b.transaction_id
        && a.version == b.version
        && a.requester_ip == b.requester_ip
        && a.read_only == b.read_only
        && match (&a.message_type, &b.message_type) {
            (MessageType::Request(a), MessageType::Request(b)) => request_eq(a, b),
            (MessageType::Response(a), MessageType::Response(b)) => response_eq(a, b),
            (MessageType::Error(a), MessageType::Error(b)) => a == b,
            _ => false,
        }
}

// ==== Properties ====

/// A well-formed ping request.
const PING_REQUEST: &[u8] = b"d1:ad2:id20:abcdefghij0123456789e1:q4:ping1:t2:aa1:y1:qe";

/// Applies random replacements, insertions and deletions to a packet.
fn mutate(base: &[u8], mutations: &[(u8, usize, u8)]) -> Vec<u8> {
    let mut bytes = base.to_vec();
    for &(operation, position, byte) in mutations {
        match operation % 3 {
            0 if !bytes.is_empty() => {
                let index = position % bytes.len();
                if let Some(slot) = bytes.get_mut(index) {
                    *slot = byte;
                }
            }
            1 => {
                let index = position % (bytes.len() + 1);
                bytes.insert(index, byte);
            }
            2 if !bytes.is_empty() => {
                let index = position % bytes.len();
                bytes.remove(index);
            }
            _ => {}
        }
    }
    bytes
}

/// Bencode keys that the decoder knows or ignores.
const MESSAGE_KEYS: &[&str] = &[
    "t",
    "v",
    "y",
    "q",
    "a",
    "r",
    "e",
    "id",
    "target",
    "info_hash",
    "nodes",
    "values",
    "token",
    "port",
    "implied_port",
    "k",
    "seq",
    "sig",
    "salt",
    "cas",
    "ip",
    "ro",
    "peers",
    "want",
    "extra",
];

/// A dictionary entry with a known DHT key and an arbitrary bencode value.
fn random_field() -> impl Strategy<Value = (String, Nested)> {
    (0usize..MESSAGE_KEYS.len(), nested_value(3)).prop_map(|(index, value)| {
        let key = MESSAGE_KEYS.get(index).copied().unwrap_or("t").to_string();
        (key, value)
    })
}

/// A syntactically valid bencode dictionary with known keys and arbitrary
/// value types. This sends type-confused data to the decoder.
fn random_message_dict() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(random_field(), 0..=6).prop_map(|mut fields| {
        fields.sort_by(|(a, _), (b, _)| a.cmp(b));
        let mut bytes = vec![b'd'];
        for (key, value) in &fields {
            bytes.extend_from_slice(format!("{}:", key.len()).as_bytes());
            bytes.extend_from_slice(key.as_bytes());
            bytes.extend_from_slice(&value.bytes);
        }
        bytes.push(b'e');
        bytes
    })
}

#[test]
fn ping_request_is_well_formed() {
    assert!(Message::from_bytes(PING_REQUEST).is_ok());
}

proptest! {
    /// The scanner must accept every well-formed value whose depth is within
    /// the limit, including byte strings that contain structural bytes.
    #[test]
    fn well_formed_bencode_is_accepted(value in nested_value(MAX_BENCODE_DEPTH)) {
        prop_assert!(
            matches!(scan_bencode_depth(&value.bytes), BencodeScan::Acceptable),
            "depth {} value was not accepted",
            value.depth
        );
    }

    /// The scanner must count the nesting depth exactly: wrapping a value in
    /// `wrappers` lists must be accepted if and only if the total depth is
    /// within the limit.
    #[test]
    fn nesting_limit_is_exact(
        value in nested_value(MAX_BENCODE_DEPTH),
        wrappers in 0usize..=MAX_BENCODE_DEPTH + 2,
    ) {
        let mut bytes = vec![b'l'; wrappers];
        bytes.extend_from_slice(&value.bytes);
        bytes.extend(std::iter::repeat_n(b'e', wrappers));
        let total_depth = value.depth + wrappers;

        let verdict = scan_bencode_depth(&bytes);
        if total_depth > MAX_BENCODE_DEPTH {
            prop_assert!(
                matches!(verdict, BencodeScan::TooDeep),
                "depth {total_depth} was not rejected"
            );
        } else {
            prop_assert!(
                matches!(verdict, BencodeScan::Acceptable),
                "depth {total_depth} was not accepted"
            );
        }
    }

    /// A well-formed message with a deeply nested unknown key must be
    /// rejected before the parser recurses. Without the guard the parser
    /// accepts the message, because it ignores the unknown key.
    #[test]
    fn well_formed_deep_message_is_rejected(
        value in nested_value(MAX_BENCODE_DEPTH),
        wrappers in 0usize..=MAX_BENCODE_DEPTH + 2,
    ) {
        let mut bytes = b"d1:ad2:id20:abcdefghij0123456789e1:q4:ping1:t2:aa1:y1:q1:x".to_vec();
        bytes.extend(std::iter::repeat_n(b'l', wrappers));
        bytes.extend_from_slice(&value.bytes);
        bytes.extend(std::iter::repeat_n(b'e', wrappers));
        bytes.push(b'e');

        // The root dictionary adds one level.
        let total_depth = value.depth + wrappers + 1;
        let result = Message::from_bytes(&bytes);
        if total_depth > MAX_BENCODE_DEPTH {
            prop_assert!(
                matches!(result, Err(DecodeMessageError::BencodeTooDeep)),
                "depth {total_depth} was not rejected"
            );
        } else {
            prop_assert!(result.is_ok(), "depth {total_depth} was rejected");
        }
    }

    /// Arbitrary bytes must not panic the decoder. The decoder must not
    /// report `BencodeTooDeep` unless the scanner reports the depth limit.
    /// A stack overflow would abort the test process.
    #[test]
    fn arbitrary_packets_are_decoded_safely(
        bytes in prop::collection::vec(any::<u8>(), 0..=4096),
    ) {
        let too_deep = matches!(scan_bencode_depth(&bytes), BencodeScan::TooDeep);
        let result = Message::from_bytes(&bytes);

        if too_deep {
            prop_assert!(result.is_err(), "too deep packet was accepted");
        } else {
            prop_assert!(
                !matches!(result, Err(DecodeMessageError::BencodeTooDeep)),
                "packet within the limit was rejected as too deep"
            );
        }
    }

    /// Mutated packets near a well-formed message must not panic the decoder.
    /// This covers the path that sends malformed input to the bencode parser.
    #[test]
    fn mutated_packets_are_decoded_safely(
        mutations in prop::collection::vec(
            (any::<u8>(), any::<usize>(), any::<u8>()),
            0..=24,
        ),
    ) {
        let bytes = mutate(PING_REQUEST, &mutations);
        let result = Message::from_bytes(&bytes);

        if matches!(scan_bencode_depth(&bytes), BencodeScan::TooDeep) {
            prop_assert!(result.is_err(), "too deep packet was accepted");
        }
    }

    /// Every message must survive a round-trip through the wire format.
    #[test]
    fn every_message_round_trips(message in any_message()) {
        let bytes = message
            .to_bytes()
            .map_err(|err| TestCaseError::fail(err.to_string()))?;
        let decoded = Message::from_bytes(&bytes)
            .map_err(|err| TestCaseError::fail(err.to_string()))?;
        prop_assert!(
            wire_eq(&message, &decoded),
            "message did not round-trip: {message:?} -> {decoded:?}"
        );
    }

    /// A decoded packet must encode to a packet that decodes to the same
    /// message. This checks that encoding is stable.
    #[test]
    fn decoded_packets_reencode_stably(
        bytes in prop::collection::vec(any::<u8>(), 0..=2048),
    ) {
        let Ok(decoded) = Message::from_bytes(&bytes) else {
            return Ok(());
        };
        let reencoded = decoded
            .to_bytes()
            .map_err(|err| TestCaseError::fail(err.to_string()))?;
        let redecode = Message::from_bytes(&reencoded)
            .map_err(|err| TestCaseError::fail(err.to_string()))?;
        prop_assert!(
            wire_eq(&decoded, &redecode),
            "re-encoding changed the message: {decoded:?} -> {redecode:?}"
        );
    }

    /// Mutating a valid encoding must not panic the decoder. If the mutated
    /// packet decodes, re-encoding it must be stable.
    #[test]
    fn mutated_encodings_reencode_stably(
        message in any_message(),
        mutations in prop::collection::vec(
            (any::<u8>(), any::<usize>(), any::<u8>()),
            0..=12,
        ),
    ) {
        let bytes = message
            .to_bytes()
            .map_err(|err| TestCaseError::fail(err.to_string()))?;
        let mutated = mutate(&bytes, &mutations);
        let Ok(decoded) = Message::from_bytes(&mutated) else {
            return Ok(());
        };
        let reencoded = decoded
            .to_bytes()
            .map_err(|err| TestCaseError::fail(err.to_string()))?;
        let redecode = Message::from_bytes(&reencoded)
            .map_err(|err| TestCaseError::fail(err.to_string()))?;
        prop_assert!(
            wire_eq(&decoded, &redecode),
            "re-encoding changed a mutated packet: {decoded:?} -> {redecode:?}"
        );
    }

    /// Truncated packets must not panic the decoder. A packet can be cut at
    /// any byte boundary when the socket buffer is too small.
    #[test]
    fn truncated_packets_are_decoded_safely(
        message in any_message(),
        keep in 0usize..=2048,
    ) {
        let bytes = message
            .to_bytes()
            .map_err(|err| TestCaseError::fail(err.to_string()))?;
        let Some(prefix) = bytes.get(..keep.min(bytes.len())) else {
            return Ok(());
        };
        let _ = Message::from_bytes(prefix);
    }

    /// Type-confused dictionaries must not panic the decoder. If one decodes,
    /// re-encoding it must be stable.
    #[test]
    fn type_confused_dictionaries_are_decoded_safely(bytes in random_message_dict()) {
        let Ok(decoded) = Message::from_bytes(&bytes) else {
            return Ok(());
        };
        let reencoded = decoded
            .to_bytes()
            .map_err(|err| TestCaseError::fail(err.to_string()))?;
        let redecode = Message::from_bytes(&reencoded)
            .map_err(|err| TestCaseError::fail(err.to_string()))?;
        prop_assert!(
            wire_eq(&decoded, &redecode),
            "re-encoding changed a type-confused packet: {decoded:?} -> {redecode:?}"
        );
    }
}
