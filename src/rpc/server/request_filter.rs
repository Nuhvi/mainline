use std::{fmt::Debug, net::SocketAddrV4};

use dyn_clone::DynClone;

use crate::RequestSpecific;

/// A trait for filtering incoming requests to a DHT node and
/// decide whether to allow handling it or rate limit or ban
/// the requester, or prohibit specific requests' details.
pub trait RequestFilter: Send + Sync + Debug + DynClone {
    /// Returns true if the request from this source is allowed.
    fn allow_request(&self, request: &RequestSpecific, from: SocketAddrV4) -> bool;
}

dyn_clone::clone_trait_object!(RequestFilter);

#[derive(Debug, Clone)]
pub(crate) struct DefaultFilter;

impl RequestFilter for DefaultFilter {
    fn allow_request(&self, _request: &RequestSpecific, _from: SocketAddrV4) -> bool {
        true
    }
}
