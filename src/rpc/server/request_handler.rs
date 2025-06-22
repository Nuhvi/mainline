use std::{collections::HashMap, fmt::Debug, net::SocketAddrV4};

use dyn_clone::DynClone;
use serde_bencode::value::Value;

use crate::rpc::server::ServerResponse;

/// A trait for handling custom RPC requests on server side.
pub trait RequestHandler: Send + Sync + Debug + DynClone {
    /// Returns None if the query is not supported,
    /// or return the response message otherwise.
    fn handle_request(
        &self,
        query: String,
        request: HashMap<String, Value>,
        from: SocketAddrV4,
    ) -> Option<ServerResponse>;
}

dyn_clone::clone_trait_object!(RequestHandler);

#[derive(Debug, Clone)]
pub(crate) struct DefaultHandler;

impl RequestHandler for DefaultHandler {
    fn handle_request(
        &self,
        _query: String,
        _request: HashMap<String, Value>,
        _from: SocketAddrV4,
    ) -> Option<ServerResponse> {
        None
    }
}
