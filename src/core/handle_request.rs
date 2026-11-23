use std::net::SocketAddrV4;
use std::time::Instant;

use tracing::info;

use crate::common::{Id, RequestTypeSpecific};
use crate::core::{Core, REQUESTER_PING_INTERVAL};
use crate::{MessageType, RequestSpecific};

impl Core {
    pub(crate) fn handle_request(
        &mut self,
        from: SocketAddrV4,
        request_from_read_only_node: bool,
        request_specific: RequestSpecific,
    ) -> (Option<MessageType>, bool) {
        let allowed = self.server_mode && self.server.allow_request(&request_specific, from);

        if allowed {
            self.maybe_ping_requester(from, request_from_read_only_node, &request_specific);
        }

        let should_repopulate_routing_tables =
            self.does_verify_our_new_public_address_with_self_ping(from, &request_specific);

        let response = if allowed {
            let server = &mut self.server;
            let response = server.handle_allowed_request(
                &self.routing_table,
                &self.signed_peers_routing_table,
                from,
                request_specific,
            );

            match response {
                Some(MessageType::Error(_) | MessageType::Response(_)) => response,
                _ => None,
            }
        } else {
            None
        };

        (response, should_repopulate_routing_tables)
    }

    /// Pings a requester that we might want in our routing tables.
    ///
    /// A request proves nothing: its sender address can be forged, and the
    /// requester picks its own id. So we never add a requester directly. We
    /// ping it instead, and its response, which the socket matches against our
    /// transaction id and the address we pinged, adds it in
    /// [`Core::handle_response`].
    ///
    /// We only do this for `find_node` requests.
    fn maybe_ping_requester(
        &mut self,
        from: SocketAddrV4,
        request_from_read_only_node: bool,
        request_specific: &RequestSpecific,
    ) {
        if !self.server_mode || request_from_read_only_node {
            return;
        }

        if !matches!(
            &request_specific.request_type,
            RequestTypeSpecific::FindNode(_)
        ) {
            return;
        }

        let known = self
            .routing_table
            .nodes()
            .any(|node| node.address() == from)
            || self
                .signed_peers_routing_table
                .nodes()
                .any(|node| node.address() == from);
        if known {
            return;
        }

        if self
            .recently_pinged
            .get(&from)
            .is_some_and(|pinged| pinged.elapsed() < REQUESTER_PING_INTERVAL)
        {
            return;
        }

        self.recently_pinged.put(from, Instant::now());
        self.pending_pings.push(from);
    }

    /// Check an incoming ping from our public address and clear `firewalled`.
    ///
    /// This is a best-effort check. It does not check the requester id or the
    /// transaction id.
    ///
    /// Returns true if the routing tables must repopulate after an id reset.
    /// This is intentional. A spoofed packet can trigger the check. NAT hairpinning
    /// can also return the self-ping while external peers cannot reach us.
    ///
    /// Accept these false positives. Server mode is safe, and the write token binds
    /// to the source IP and target. If we are unreachable, we receive no inbound
    /// requests. Peers that add us only spend probes until they evict us. A blocked
    /// hairpin path gives a false negative; in that case, start the node in server mode.
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
                        "Our current id {} is not valid for address {}. Using new id {}",
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;
    use crate::common::{
        FindNodeRequestArguments, Message, PingResponseArguments, ResponseSpecific,
    };
    use crate::{RequestFilter, ServerSettings};

    fn new_core(server_mode: bool, server_settings: ServerSettings) -> Core {
        Core::new(Id::random(), vec![], server_mode, server_settings)
    }

    fn find_node(core: &mut Core, from: SocketAddrV4, read_only: bool) {
        let _ = core.handle_request(
            from,
            read_only,
            RequestSpecific {
                requester_id: Id::from_ipv4(*from.ip()),
                request_type: RequestTypeSpecific::FindNode(FindNodeRequestArguments {
                    target: Id::random(),
                }),
            },
        );
    }

    fn ping_response(core: &mut Core, from: SocketAddrV4, responder_id: Id) {
        let _ = core.handle_response(
            from,
            Message {
                transaction_id: 0_u32.to_be_bytes().into(),
                version: None,
                requester_ip: None,
                read_only: false,
                message_type: MessageType::Response(ResponseSpecific::Ping(
                    PingResponseArguments { responder_id },
                )),
            },
        );
    }

    #[test]
    fn accepted_find_node_pings_requester_and_does_not_add() {
        let from = SocketAddrV4::new([203, 0, 113, 10].into(), 6881);
        let mut core = new_core(true, ServerSettings::default());

        find_node(&mut core, from, false);
        assert!(core.routing_table.is_empty());
        assert_eq!(core.take_pending_pings(), vec![from]);

        // Repeated requests do not trigger another ping.
        find_node(&mut core, from, false);
        assert!(core.take_pending_pings().is_empty());

        let id = Id::from_ipv4(*from.ip());
        ping_response(&mut core, from, id);

        let nodes: Vec<_> = core.routing_table.nodes().collect();
        assert_eq!(nodes.len(), 1);
        let node = nodes.first();
        assert_eq!(node.map(|node| *node.id()), Some(id));
        assert_eq!(node.map(|node| node.address()), Some(from));
    }

    #[test]
    fn read_only_requesters_are_not_pinged() {
        let from = SocketAddrV4::new([203, 0, 113, 10].into(), 6881);
        let mut core = new_core(true, ServerSettings::default());

        find_node(&mut core, from, true);

        assert!(core.take_pending_pings().is_empty());
        assert!(core.routing_table.is_empty());
    }

    #[test]
    fn client_mode_does_not_ping() {
        let from = SocketAddrV4::new([203, 0, 113, 10].into(), 6881);
        let mut core = new_core(false, ServerSettings::default());

        find_node(&mut core, from, false);

        assert!(core.take_pending_pings().is_empty());
    }

    #[test]
    fn known_requesters_are_not_pinged() {
        let from = SocketAddrV4::new([203, 0, 113, 10].into(), 6881);
        let mut core = new_core(true, ServerSettings::default());

        ping_response(&mut core, from, Id::from_ipv4(*from.ip()));
        find_node(&mut core, from, false);

        assert!(core.take_pending_pings().is_empty());
    }

    #[test]
    fn rejected_requests_do_not_ping() {
        #[derive(Clone, Debug)]
        struct RejectAll;

        impl RequestFilter for RejectAll {
            fn allow_request(&self, _: &RequestSpecific, _: SocketAddrV4) -> bool {
                false
            }
        }

        let from = SocketAddrV4::new([203, 0, 113, 10].into(), 6881);
        let mut core = new_core(
            true,
            ServerSettings {
                filter: Box::new(RejectAll),
                ..Default::default()
            },
        );

        find_node(&mut core, from, false);

        assert!(core.take_pending_pings().is_empty());
        assert!(core.routing_table.is_empty());
    }

    #[test]
    fn request_filter_is_called_once() {
        #[derive(Clone, Debug)]
        struct CountingFilter(Arc<AtomicUsize>);

        impl RequestFilter for CountingFilter {
            fn allow_request(&self, _: &RequestSpecific, _: SocketAddrV4) -> bool {
                self.0.fetch_add(1, Ordering::Relaxed);
                true
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let from = SocketAddrV4::new([203, 0, 113, 10].into(), 6881);
        let mut core = new_core(
            true,
            ServerSettings {
                filter: Box::new(CountingFilter(calls.clone())),
                ..Default::default()
            },
        );

        find_node(&mut core, from, false);

        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
}
