//! UDP socket layer managing incoming/outgoing requests and responses.

use std::fmt::Debug;
use std::io::ErrorKind;
use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};
use tracing::{debug, trace, warn};

use crate::common::{ErrorSpecific, Message, MessageType, RequestSpecific, ResponseSpecific};
#[cfg(not(test))]
use crate::core::VERSION;

use super::config::Config;

const MTU: usize = 2048;

pub const DEFAULT_PORT: u16 = 6881;

/// Minimum interval between polling udp socket, lower latency, higher cpu usage.
/// Useful for checking for expected responses.
pub const MIN_POLL_INTERVAL: Duration = Duration::from_micros(100);
/// Maximum interval between polling udp socket, higher latency, lower cpu usage.
/// Useful for waiting for incoming requests, and [super::Actor::periodic_node_maintaenance].
pub const MAX_POLL_INTERVAL: Duration = Duration::from_millis(500);
pub const MIN_REQUEST_TIMEOUT: Duration = Duration::from_millis(500);

/// Version before supporting `announce_signed_peers`
#[cfg(test)]
const LEGACY_VERSION: [u8; 4] = [82, 83, 0, 5]; // "RS" version 05

/// A UdpSocket wrapper that formats and correlates DHT requests and responses.
#[derive(Debug)]
pub struct KrpcSocket {
    socket: UdpSocket,
    pub(crate) server_mode: bool,
    local_addr: SocketAddrV4,

    inflight_requests: InflightRequests,
    poll_interval: Duration,

    #[cfg(test)]
    version: [u8; 4],
}

impl KrpcSocket {
    pub(crate) fn new(config: &Config) -> Result<Self, std::io::Error> {
        let port = config.port;

        let socket = if let Some(port) = port {
            UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], port)))?
        } else {
            match UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT))) {
                Ok(socket) => Ok(socket),
                Err(_) => UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0))),
            }?
        };

        let local_addr = match socket.local_addr()? {
            SocketAddr::V4(addr) => addr,
            SocketAddr::V6(_) => unimplemented!("KrpcSocket does not support Ipv6"),
        };

        socket.set_read_timeout(Some(MIN_POLL_INTERVAL))?;

        Ok(Self {
            socket,
            server_mode: config.server_mode,
            inflight_requests: InflightRequests::new(),
            local_addr,
            poll_interval: MIN_POLL_INTERVAL,

            #[cfg(test)]
            version: if config.disable_announce_signed_peers {
                LEGACY_VERSION
            } else {
                crate::core::VERSION
            },
        })
    }

    #[cfg(test)]
    pub(crate) fn server() -> Result<Self, std::io::Error> {
        Self::new(&Config {
            server_mode: true,
            ..Default::default()
        })
    }

    #[cfg(test)]
    pub(crate) fn client() -> Result<Self, std::io::Error> {
        Self::new(&Config::default())
    }

    // === Getters ===

    /// Returns the address the server is listening to.
    #[inline]
    pub fn local_addr(&self) -> SocketAddrV4 {
        self.local_addr
    }

    // === Public Methods ===

    /// Returns true if this message's transaction_id is still inflight
    pub fn inflight(&self, transaction_id: &u32) -> bool {
        self.inflight_requests.get(*transaction_id).is_some()
    }

    /// Send a request to the given address and return the transaction_id
    pub fn request(&mut self, address: SocketAddrV4, request: RequestSpecific) -> u32 {
        let transaction_id = self.inflight_requests.add(address);

        let message = self.request_message(transaction_id, request);
        trace!(context = "socket_message_sending", message = ?message);

        let tid = message.transaction_id;
        let _ = self.send(address, message).map_err(|e| {
            debug!(?e, "Error sending request message");
        });

        self.poll_interval = MIN_POLL_INTERVAL;
        let _ = self.socket.set_read_timeout(Some(self.poll_interval));

        tid
    }

    /// Send a response to the given address.
    pub fn response(
        &mut self,
        address: SocketAddrV4,
        transaction_id: u32,
        response: ResponseSpecific,
    ) {
        let message =
            self.response_message(MessageType::Response(response), address, transaction_id);
        trace!(context = "socket_message_sending", message = ?message);
        let _ = self.send(address, message).map_err(|e| {
            debug!(?e, "Error sending response message");
        });
    }

    /// Send an error to the given address.
    pub fn error(&mut self, address: SocketAddrV4, transaction_id: u32, error: ErrorSpecific) {
        let message = self.response_message(MessageType::Error(error), address, transaction_id);
        let _ = self.send(address, message).map_err(|e| {
            debug!(?e, "Error sending error message");
        });
    }

    /// Receives a single krpc message on the socket.
    /// On success, returns the dht message and the origin.
    pub fn recv_from(&mut self) -> Option<(Message, SocketAddrV4)> {
        let mut buf = [0u8; MTU];

        self.inflight_requests.cleanup();

        match self.socket.recv_from(&mut buf) {
            Ok((amt, SocketAddr::V4(from))) => {
                if self.poll_interval > MIN_POLL_INTERVAL {
                    // More eagerness if we are expecting responses than requests;
                    if !self.inflight_requests.is_empty() {
                        self.poll_interval = MIN_POLL_INTERVAL;
                    } else if self.server_mode {
                        self.poll_interval = (self.poll_interval / 2).max(MIN_POLL_INTERVAL);
                    }
                    let _ = self.socket.set_read_timeout(Some(self.poll_interval));
                }

                let bytes = &buf[..amt];

                if from.port() == 0 {
                    trace!(
                        context = "socket_validation",
                        message = "Response from port 0"
                    );
                    return None;
                }

                match Message::from_bytes(bytes) {
                    Ok(message) => {
                        // Parsed correctly.
                        let should_return = match message.message_type {
                            MessageType::Request(ref _request_specific) => {
                                // simulate legacy nodes not supporting `announce_signed_peers` and `get_signed_peers`
                                #[cfg(test)]
                                let should_return =
                                    supports_request(&self.version, _request_specific);
                                #[cfg(not(test))]
                                let should_return = true;

                                trace!(
                                    context = "socket_message_receiving",
                                    ?message,
                                    ?from,
                                    "Received request message"
                                );

                                should_return
                            }
                            MessageType::Response(_) => {
                                trace!(
                                    context = "socket_message_receiving",
                                    ?message,
                                    ?from,
                                    "Received response message"
                                );

                                self.is_expected_response(&message, &from)
                            }
                            MessageType::Error(_) => {
                                trace!(
                                    context = "socket_message_receiving",
                                    ?message,
                                    ?from,
                                    "Received error message"
                                );

                                self.is_expected_response(&message, &from)
                            }
                        };

                        if should_return {
                            return Some((message, from));
                        }
                    }
                    Err(error) => {
                        trace!(context = "socket_error", ?error, ?from, message = ?String::from_utf8_lossy(bytes), "Received invalid Bencode message.");
                    }
                };
            }
            Ok((_, SocketAddr::V6(_))) => {
                // Ignore unsupported Ipv6 messages
            }
            Err(error) => match error.kind() {
                ErrorKind::WouldBlock => {
                    if self.poll_interval < MAX_POLL_INTERVAL {
                        self.poll_interval =
                            (self.poll_interval.mul_f32(1.1)).min(MAX_POLL_INTERVAL);
                        let _ = self.socket.set_read_timeout(Some(self.poll_interval));

                        // Too noisy, only uncomment when you suspect there is a performance issue..
                        // trace!("Increased poll_interval {:?}", self.poll_interval);
                    }
                }
                _ => {
                    warn!("IO error {error}")
                }
            },
        };

        None
    }

    // === Private Methods ===

    fn is_expected_response(&mut self, message: &Message, from: &SocketAddrV4) -> bool {
        // Positive or an error response or to an inflight request.
        match self.inflight_requests.remove(message.transaction_id) {
            Some(request) => {
                if compare_socket_addr(&request.to, from) {
                    return true;
                } else {
                    trace!(
                        context = "socket_validation",
                        message = "Response from wrong address"
                    );
                }
            }
            None => {
                trace!(
                    context = "socket_validation",
                    message = "Unexpected response id, or timedout request"
                );
            }
        }

        false
    }

    /// Set transactin_id, version and read_only
    fn request_message(&mut self, transaction_id: u32, message: RequestSpecific) -> Message {
        Message {
            transaction_id,
            message_type: MessageType::Request(message),
            version: Some(self.version()),
            read_only: !self.server_mode,
            requester_ip: None,
        }
    }

    /// Same as request_message but with request transaction_id and the requester_ip.
    fn response_message(
        &mut self,
        message: MessageType,
        requester_ip: SocketAddrV4,
        request_tid: u32,
    ) -> Message {
        Message {
            transaction_id: request_tid,
            message_type: message,
            version: Some(self.version()),
            read_only: !self.server_mode,
            // BEP_0042 Only relevant in responses.
            requester_ip: Some(requester_ip),
        }
    }

    fn version(&self) -> [u8; 4] {
        #[cfg(test)]
        return self.version;
        #[cfg(not(test))]
        return VERSION;
    }

    /// Send a raw dht message
    fn send(&mut self, address: SocketAddrV4, message: Message) -> Result<(), SendMessageError> {
        self.socket.send_to(&message.to_bytes()?, address)?;
        trace!(context = "socket_message_sending", message = ?message);
        Ok(())
    }
}

#[derive(thiserror::Error, Debug)]
/// Mainline crate error enum.
pub enum SendMessageError {
    /// Errors related to parsing DHT messages.
    #[error("Failed to parse packet bytes: {0}")]
    BencodeError(#[from] serde_bencode::Error),

    #[error(transparent)]
    /// Transparent [std::io::Error]
    IO(#[from] std::io::Error),
}

// Same as SocketAddr::eq but ignores the ip if it is unspecified for testing reasons.
fn compare_socket_addr(a: &SocketAddrV4, b: &SocketAddrV4) -> bool {
    if a.port() != b.port() {
        return false;
    }

    if a.ip().is_unspecified() {
        return true;
    }

    a.ip() == b.ip()
}

pub struct InflightRequest {
    tid: u32,
    to: SocketAddrV4,
    sent_at: Instant,
}

impl Debug for InflightRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InflightRequest")
            .field("tid", &self.tid)
            .field("to", &self.to)
            .field("elapsed", &self.sent_at.elapsed())
            .finish()
    }
}

/// We don't need a map, since we know the maximum size is `65536` requests.
/// Requests are also ordered by their transaction_id and thus sent_at, so lookup is fast.
struct InflightRequests {
    next_tid: u32,
    requests: Vec<InflightRequest>,
    estimated_rtt: Duration,
    deviation_rtt: Duration,
}

impl InflightRequests {
    fn new() -> Self {
        Self {
            next_tid: 0,
            requests: Vec::new(),
            estimated_rtt: MIN_REQUEST_TIMEOUT,
            deviation_rtt: Duration::from_secs(0),
        }
    }

    /// Increments self.next_tid and returns the previous value.
    fn tid(&mut self) -> u32 {
        // We don't reuse freed transaction ids, to preserve the sortablitiy
        // of both `tid`s and `sent_at`.
        let tid = self.next_tid;
        self.next_tid = self.next_tid.wrapping_add(1);
        tid
    }

    fn request_timeout(&self) -> Duration {
        self.estimated_rtt + self.deviation_rtt.mul_f64(4.0)
    }

    fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    fn get(&self, key: u32) -> Option<&InflightRequest> {
        if let Ok(index) = self.find_by_tid(key) {
            if let Some(request) = self.requests.get(index) {
                if request.sent_at.elapsed() < self.request_timeout() {
                    return Some(request);
                }
            };
        }

        None
    }

    /// Adds a [InflightRequest] with new transaction_id, and returns that id.
    fn add(&mut self, to: SocketAddrV4) -> u32 {
        let tid = self.tid();
        self.requests.push(InflightRequest {
            tid,
            to,
            sent_at: Instant::now(),
        });

        tid
    }

    fn remove(&mut self, key: u32) -> Option<InflightRequest> {
        match self.find_by_tid(key) {
            Ok(index) => {
                let request = self.requests.remove(index);

                self.update_rtt_estimates(request.sent_at.elapsed());
                trace!(
                    "Updated estimated round trip time {:?} and request timeout {:?}",
                    self.estimated_rtt,
                    self.request_timeout()
                );

                Some(request)
            }
            Err(_) => None,
        }
    }

    fn update_rtt_estimates(&mut self, sample_rtt: Duration) {
        if sample_rtt < MIN_REQUEST_TIMEOUT {
            return;
        }

        // Use TCP-like alpha = 1/8, beta = 1/4
        let alpha = 0.125;
        let beta = 0.25;

        let sample_rtt_secs = sample_rtt.as_secs_f64();
        let est_rtt_secs = self.estimated_rtt.as_secs_f64();
        let dev_rtt_secs = self.deviation_rtt.as_secs_f64();

        let new_est_rtt = (1.0 - alpha) * est_rtt_secs + alpha * sample_rtt_secs;
        let new_dev_rtt =
            (1.0 - beta) * dev_rtt_secs + beta * (sample_rtt_secs - new_est_rtt).abs();

        self.estimated_rtt = Duration::from_secs_f64(new_est_rtt);
        self.deviation_rtt = Duration::from_secs_f64(new_dev_rtt);
    }

    fn find_by_tid(&self, tid: u32) -> Result<usize, usize> {
        self.requests
            .binary_search_by(|request| request.tid.cmp(&tid))
    }

    /// Removes timeedout requests if necessary to save memory
    fn cleanup(&mut self) {
        if self.requests.len() < self.requests.capacity() {
            return;
        }

        let index = match self
            .requests
            .binary_search_by(|request| self.request_timeout().cmp(&request.sent_at.elapsed()))
        {
            Ok(index) => index,
            Err(index) => index,
        };

        self.requests.drain(0..index);
    }
}

impl Debug for InflightRequests {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let timeout = self.request_timeout();
        let non_expired: Vec<_> = self
            .requests
            .iter()
            .filter(|req| req.sent_at.elapsed() < timeout)
            .collect();

        f.debug_struct("InflightRequests")
            .field("next_tid", &self.next_tid)
            .field("requests", &non_expired)
            .field("estimated_rtt", &self.estimated_rtt)
            .field("deviation_rtt", &self.deviation_rtt)
            .field("request_timeout", &timeout)
            .finish()
    }
}

#[cfg(test)]
fn supports_request(version: &[u8; 4], request_specific: &crate::common::RequestSpecific) -> bool {
    use crate::{
        common::{PutRequest, RequestTypeSpecific},
        PutRequestSpecific,
    };
    match request_specific.request_type {
        RequestTypeSpecific::GetSignedPeers(_)
        | RequestTypeSpecific::Put(PutRequest {
            put_request_type: PutRequestSpecific::AnnounceSignedPeer(_),
            ..
        }) => version != &LEGACY_VERSION,
        _ => true,
    }
}

#[cfg(test)]
mod test {
    use std::thread;

    use crate::{
        common::{Id, PingResponseArguments, RequestTypeSpecific},
        core::VERSION,
    };

    use super::*;

    #[test]
    fn tid() {
        let mut socket = KrpcSocket::server().unwrap();

        assert_eq!(socket.inflight_requests.tid(), 0);
        assert_eq!(socket.inflight_requests.tid(), 1);
        assert_eq!(socket.inflight_requests.tid(), 2);

        socket.inflight_requests.next_tid = u32::MAX;

        assert_eq!(socket.inflight_requests.tid(), u32::MAX);
        assert_eq!(socket.inflight_requests.tid(), 0);
    }

    #[test]
    fn recv_request() {
        let mut server = KrpcSocket::server().unwrap();
        let server_address = server.local_addr();

        let mut client = KrpcSocket::client().unwrap();
        client.inflight_requests.next_tid = 120;

        let client_address = client.local_addr();
        let request = RequestSpecific {
            requester_id: Id::random(),
            request_type: RequestTypeSpecific::Ping,
        };

        let expected_request = request.clone();

        let server_thread = thread::spawn(move || loop {
            if let Some((message, from)) = server.recv_from() {
                assert_eq!(from.port(), client_address.port());
                assert_eq!(message.transaction_id, 120);
                assert!(message.read_only, "Read-only should be true");
                assert_eq!(message.version, Some(VERSION), "Version should be 'RS'");
                assert_eq!(message.message_type, MessageType::Request(expected_request));
                break;
            }
        });

        client.request(server_address, request);

        server_thread.join().unwrap();
    }

    #[test]
    fn recv_response() {
        let (tx, rx) = flume::bounded(1);

        let mut client = KrpcSocket::client().unwrap();
        let client_address = client.local_addr();

        let responder_id = Id::random();
        let response = ResponseSpecific::Ping(PingResponseArguments { responder_id });

        let server_thread = thread::spawn(move || {
            let mut server = KrpcSocket::client().unwrap();
            let server_address = server.local_addr();
            tx.send(server_address).unwrap();

            server.inflight_requests.requests.push(InflightRequest {
                tid: 8,
                to: client_address,
                sent_at: Instant::now(),
            });

            loop {
                if let Some((message, from)) = server.recv_from() {
                    assert_eq!(from.port(), client_address.port());
                    assert_eq!(message.transaction_id, 8);
                    assert!(message.read_only, "Read-only should be true");
                    assert_eq!(message.version, Some(VERSION), "Version should be 'RS'");
                    assert_eq!(
                        message.message_type,
                        MessageType::Response(ResponseSpecific::Ping(PingResponseArguments {
                            responder_id,
                        }))
                    );
                    break;
                }
            }
        });

        let server_address = rx.recv().unwrap();

        client.response(server_address, 8, response);

        server_thread.join().unwrap();
    }

    #[test]
    fn ignore_response_from_wrong_address() {
        let mut server = KrpcSocket::client().unwrap();
        let server_address = server.local_addr();

        let mut client = KrpcSocket::client().unwrap();

        let client_address = client.local_addr();

        server.inflight_requests.requests.push(InflightRequest {
            tid: 8,
            to: SocketAddrV4::new([127, 0, 0, 1].into(), client_address.port() + 1),
            sent_at: Instant::now(),
        });

        let response = ResponseSpecific::Ping(PingResponseArguments {
            responder_id: Id::random(),
        });

        let _ = response.clone();

        let server_thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(5));
            assert!(
                server.recv_from().is_none(),
                "Should not receive a response from wrong address"
            );
        });

        client.response(server_address, 8, response);

        server_thread.join().unwrap();
    }

    #[test]
    fn inflight_request_timeout() {
        let mut server = KrpcSocket::client().unwrap();

        let tid = 8;
        let sent_at = Instant::now();

        server.inflight_requests.requests.push(InflightRequest {
            tid,
            to: SocketAddrV4::new([0, 0, 0, 0].into(), 0),
            sent_at,
        });

        std::thread::sleep(server.inflight_requests.request_timeout());

        assert!(!server.inflight(&tid));
    }
}
