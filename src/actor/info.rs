use std::net::SocketAddrV4;

use crate::{DhtSizeEstimate, Id};

use super::Actor;

/// Information and statistics about this mainline node.
#[derive(Debug, Clone)]
pub struct Info {
    id: Id,
    local_addr: SocketAddrV4,
    public_address: Option<SocketAddrV4>,
    firewalled: bool,
    dht_size_estimate: DhtSizeEstimate,
    server_mode: bool,

    routing_table_size: usize,
    signed_peers_routing_table_size: usize,
}

impl Info {
    /// This Node's [Id]
    #[must_use]
    pub fn id(&self) -> &Id {
        &self.id
    }
    /// Local UDP IPv4 socket address that this node is listening on.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddrV4 {
        self.local_addr
    }
    /// Returns the best guess for this node's Public address.
    ///
    /// If [`crate::DhtBuilder::public_ip`] was set, this is what will be returned
    /// (plus the local port), otherwise it will rely on consensus from
    /// responding nodes voting on our public IP and port.
    #[must_use]
    pub fn public_address(&self) -> Option<SocketAddrV4> {
        self.public_address
    }
    /// Returns `true` if we cannot confirm that [`Self::public_address`] is publicly addressable.
    ///
    /// This is a best-effort flag, not a reachability proof. NAT hairpinning can make it
    /// false when external peers cannot reach us. A blocked hairpin path can make it true
    /// when they can.
    ///
    /// A false positive mainly costs other nodes. They may add us to their routing tables,
    /// send probes, and evict us after their stale timeout. If we are unreachable, we
    /// receive no inbound requests. A false positive does not allow forged writes, because
    /// tokens bind to the source IP and target.
    ///
    /// In adaptive mode, `true` prevents the switch to server mode. An explicit
    /// [`crate::DhtBuilder::server_mode`] ignores this flag. If the node is port-forwarded
    /// or public, start it in server mode.
    #[must_use]
    pub fn firewalled(&self) -> bool {
        self.firewalled
    }

    /// Returns whether or not this node is running in server mode.
    #[must_use]
    pub fn server_mode(&self) -> bool {
        self.server_mode
    }

    /// Return the estimated DHT size and the standard deviation of the estimate.
    ///
    /// [Read more](https://github.com/nuhvi/mainline/blob/main/docs/dht_size_estimate.md)
    #[must_use]
    pub fn dht_size_estimate(&self) -> DhtSizeEstimate {
        self.dht_size_estimate
    }

    /// Returns the size of the main routing table
    #[must_use]
    pub fn routing_table_size(&self) -> usize {
        self.routing_table_size
    }

    /// Returns the size of the routing table of nodes that support the signed peers BEP.
    #[must_use]
    pub fn signed_peers_routing_table_size(&self) -> usize {
        self.signed_peers_routing_table_size
    }
}

impl From<&Actor> for Info {
    fn from(actor: &Actor) -> Self {
        Self {
            id: *actor.id(),
            public_address: actor.core.public_address,
            firewalled: actor.core.firewalled,
            server_mode: actor.core.server_mode,
            local_addr: actor.socket.local_addr(),
            dht_size_estimate: actor.core.routing_table.dht_size_estimate(),
            routing_table_size: actor.core.routing_table.size(),
            signed_peers_routing_table_size: actor.core.signed_peers_routing_table.size(),
        }
    }
}
