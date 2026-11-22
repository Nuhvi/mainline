use tracing::{debug, trace};

use crate::{
    common::{
        ErrorSpecific, Id, PutRequest, PutRequestSpecific, RequestSpecific, RequestTypeSpecific,
    },
    Node,
};

use crate::actor::socket::KrpcSocket;

#[derive(Debug)]
/// Once an [`super::IterativeQuery`] is done, or if a previous cached one was a available,
/// we can store data at the closest nodes using this `PutQuery`, that keeps track of
/// acknowledging nodes, and or errors.
pub(crate) struct PutQuery {
    pub target: Id,
    /// Nodes that confirmed success
    stored_at: usize,
    inflight_requests: Vec<u32>,
    pub request: PutRequestSpecific,
    errors: Vec<(usize, ErrorSpecific)>,
    extra_nodes: Box<[Node]>,
}

impl PutQuery {
    pub(crate) fn new(request: PutRequestSpecific, extra_nodes: Option<Box<[Node]>>) -> Self {
        Self {
            target: *request.target(),
            stored_at: 0,
            inflight_requests: Vec::new(),
            request,
            errors: Vec::new(),
            extra_nodes: extra_nodes.unwrap_or(Box::new([])),
        }
    }

    pub(crate) fn start(
        &mut self,
        socket: &mut KrpcSocket,
        closest_nodes: &[Node],
    ) -> Result<(), PutError> {
        if self.started() {
            return Ok(());
        }

        let target = self.target;
        debug!(?target, closest_secure_nodes = ?closest_nodes.len(), "PutQuery start");

        if closest_nodes.is_empty() {
            Err(PutQueryError::NoClosestNodes)?;
        }

        for node in closest_nodes.iter().chain(self.extra_nodes.iter()) {
            // Set correct values to the request placeholders
            if let Some(token) = node.token() {
                let tid = socket.request(
                    node.address(),
                    RequestSpecific {
                        requester_id: Id::random(),
                        request_type: RequestTypeSpecific::Put(PutRequest {
                            token,
                            put_request_type: self.request.clone(),
                        }),
                    },
                );

                self.inflight_requests.push(tid);
            }
        }

        Ok(())
    }

    pub(crate) fn started(&self) -> bool {
        !self.inflight_requests.is_empty()
    }

    pub(crate) fn inflight(&self, tid: u32) -> bool {
        self.inflight_requests.contains(&tid)
    }

    pub(crate) fn success(&mut self) {
        trace!(target = ?self.target, "PutQuery got success response");
        self.stored_at += 1;
    }

    pub(crate) fn error(&mut self, error: ErrorSpecific) {
        debug!(target = ?self.target, ?error, "PutQuery got error");

        if let Some(pos) = self
            .errors
            .iter()
            .position(|(_, err)| error.code == err.code)
        {
            // Increment the count of the existing error
            if let Some((count, _)) = self.errors.get_mut(pos) {
                *count += 1;
            }

            // Move the updated element to maintain the order (highest count first)
            let mut i = pos;
            while i > 0
                && self
                    .errors
                    .get(i)
                    .zip(self.errors.get(i - 1))
                    .is_some_and(|(a, b)| a.0 > b.0)
            {
                self.errors.swap(i, i - 1);
                i -= 1;
            }
        } else {
            // Add the new error with a count of 1
            self.errors.push((1, error));
        }
    }

    /// Check if the query is either successfully done, or terminated with an error.
    pub(crate) fn check(&self, socket: &KrpcSocket) -> Result<bool, PutError> {
        // And all queries got responses or timedout
        if self.is_done(socket) {
            let target = self.target;

            if self.stored_at == 0 {
                let most_common_error = self.most_common_error();

                debug!(
                    ?target,
                    ?most_common_error,
                    nodes_count = self.inflight_requests.len(),
                    "Put Query: failed"
                );

                return Err(
                    most_common_error.map_or(PutQueryError::Timeout.into(), |(_, error)| error)
                );
            }

            debug!(?target, stored_at = ?self.stored_at, "PutQuery Done successfully");

            return Ok(true);
        } else if let Some(most_common_error) = self.majority_nodes_rejected_put_mutable() {
            let target = self.target;

            debug!(
                ?target,
                ?most_common_error,
                nodes_count = self.inflight_requests.len(),
                "PutQuery for MutableItem was rejected by most nodes with 3xx code."
            );

            Err(most_common_error)?;
        }

        Ok(false)
    }

    /// Query started and finished
    fn is_done(&self, socket: &KrpcSocket) -> bool {
        // Didn't start yet.
        if self.inflight_requests.is_empty() {
            return false;
        }

        !self
            .inflight_requests
            .iter()
            .any(|tid| socket.inflight(*tid))
    }

    /// Return most common error if any
    fn majority_nodes_rejected_put_mutable(&self) -> Option<ConcurrencyError> {
        let half = (self.inflight_requests.len() / 2) + 1;

        if matches!(self.request, PutRequestSpecific::PutMutable(_)) {
            return self.most_common_error().and_then(|(count, error)| {
                if count >= half {
                    if let PutError::Concurrency(err) = error {
                        Some(err)
                    } else {
                        None
                    }
                } else {
                    None
                }
            });
        }

        None
    }

    fn most_common_error(&self) -> Option<(usize, PutError)> {
        self.errors.first().map(|(count, error)| {
            let error = match (&self.request, error.code) {
                (PutRequestSpecific::PutMutable(_), 301) => ConcurrencyError::CasFailed.into(),
                (PutRequestSpecific::PutMutable(_), 302) => ConcurrencyError::NotMostRecent.into(),
                _ => PutQueryError::ErrorResponse(error.clone()).into(),
            };
            (*count, error)
        })
    }
}

#[derive(thiserror::Error, Debug, Clone)]
/// `PutQuery` errors
pub enum PutError {
    /// Common `PutQuery` errors
    #[error(transparent)]
    Query(#[from] PutQueryError),

    #[error(transparent)]
    /// `PutQuery` for [`crate::MutableItem`] errors
    Concurrency(#[from] ConcurrencyError),
}

#[derive(thiserror::Error, Debug, Clone)]
/// Common `PutQuery` errors
pub enum PutQueryError {
    /// Failed to find any nodes close, usually means dht node failed to bootstrap,
    /// so the routing table is empty. Check the machine's access to UDP socket,
    /// or find better bootstrapping nodes.
    #[error("Failed to find any nodes close to store value at")]
    NoClosestNodes,

    /// Either Put Query failed to store at any nodes, and most nodes responded
    /// with a non `301` nor `302` errors.
    ///
    /// Either way; contains the most common error response.
    #[error("Query Error Response")]
    ErrorResponse(ErrorSpecific),

    /// `PutQuery` timed out with no responses neither success or errors
    #[error("PutQuery timed out with no responses neither success or errors")]
    Timeout,
}

#[derive(thiserror::Error, Debug, Clone)]
/// `PutQuery` for [`crate::MutableItem`] errors
pub enum ConcurrencyError {
    /// Trying to PUT mutable items with the same `key`, and `salt` but different `seq`.
    ///
    /// Moreover, the more recent item does _NOT_ mention the the earlier
    /// item's `seq` in its `cas` field.
    ///
    /// This risks a [Lost Update Problem](https://en.wikipedia.org/wiki/Write-write_conflict).
    ///
    /// Try reading most recent mutable item before writing again,
    /// and make sure to set the `cas` field.
    #[error("Conflict risk, try reading most recent item before writing again.")]
    ConflictRisk,

    /// The [`crate::MutableItem::seq`] is less than or equal the sequence from another signed item.
    ///
    /// Try reading most recent mutable item before writing again.
    #[error("MutableItem::seq is not the most recent, try reading most recent item before writing again.")]
    NotMostRecent,

    /// The `CAS` condition does not match the `seq` of the most recent known signed item.
    #[error("CAS check failed, try reading most recent item before writing again.")]
    CasFailed,
}

#[cfg(test)]
mod tests {
    use eyre::Result;

    use super::*;
    use crate::{
        common::{AnnouncePeerRequestArguments, PutMutableRequestArguments},
        MutableItem,
    };

    fn announce_request() -> PutRequestSpecific {
        PutRequestSpecific::AnnouncePeer(AnnouncePeerRequestArguments {
            info_hash: Id::random(),
            port: 6881,
            implied_port: None,
        })
    }

    fn mutable_request() -> PutRequestSpecific {
        let item = MutableItem::new(
            &ed25519_dalek::SigningKey::from_bytes(&[7; 32]),
            b"value",
            1,
            None,
        );
        PutRequestSpecific::PutMutable(PutMutableRequestArguments::from(item, None))
    }

    fn cas_error() -> ErrorSpecific {
        ErrorSpecific {
            code: 301,
            description: "CAS mismatch".into(),
        }
    }

    #[test]
    fn concurrency_errors_only_for_mutable_puts() -> Result<()> {
        let mut announce = PutQuery::new(announce_request(), None);
        announce.error(cas_error());
        let (count, error) = announce
            .most_common_error()
            .ok_or_else(|| eyre::eyre!("expected an error"))?;
        assert_eq!(count, 1);
        assert!(matches!(
            error,
            PutError::Query(PutQueryError::ErrorResponse(_))
        ));

        let mut mutable = PutQuery::new(mutable_request(), None);
        mutable.error(cas_error());
        let (count, error) = mutable
            .most_common_error()
            .ok_or_else(|| eyre::eyre!("expected an error"))?;
        assert_eq!(count, 1);
        assert!(matches!(
            error,
            PutError::Concurrency(ConcurrencyError::CasFailed)
        ));
        Ok(())
    }

    #[test]
    fn counts_are_not_u8() -> Result<()> {
        let mut query = PutQuery::new(announce_request(), None);

        for _ in 0..300 {
            query.error(ErrorSpecific {
                code: 500,
                description: "error".into(),
            });
        }
        let (count, error) = query
            .most_common_error()
            .ok_or_else(|| eyre::eyre!("expected an error"))?;
        assert_eq!(count, 300);
        assert!(matches!(
            error,
            PutError::Query(PutQueryError::ErrorResponse(_))
        ));

        for _ in 0..300 {
            query.success();
        }
        assert_eq!(query.stored_at, 300);
        Ok(())
    }
}
