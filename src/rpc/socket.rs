//! Udp socket layer managine incoming/outgoing requests and responses.

use std::cmp::Ordering;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};
use tracing::{debug, trace};

use crate::common::{ErrorSpecific, Message, MessageType, RequestSpecific, ResponseSpecific};

const VERSION: [u8; 4] = [82, 83, 0, 4]; // "RS" version 04
const MTU: usize = 2048;

pub const DEFAULT_PORT: u16 = 6881;
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_millis(2000); // 2 seconds
pub const READ_TIMEOUT: Duration = Duration::from_millis(10);

/// A UdpSocket wrapper that formats and correlates DHT requests and responses.
#[derive(Debug)]
pub struct KrpcSocket {
    next_tid: u16,
    socket: UdpSocket,
    read_only: bool,
    request_timeout: Duration,
    /// We don't need a HashMap, since we know the capacity is `65536` requests.
    /// Requests are also ordered by their transaction_id and thus sent_at, so lookup is fast.
    inflight_requests: Vec<InflightRequest>,
}

#[derive(Debug)]
pub struct InflightRequest {
    tid: u16,
    to: SocketAddr,
    sent_at: Instant,
}

impl KrpcSocket {
    pub(crate) fn new(
        read_only: bool,
        request_timeout: Option<Duration>,
        port: Option<u16>,
    ) -> Result<Self, std::io::Error> {
        let socket = if let Some(port) = port {
            UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], port)))?
        } else {
            match UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT))) {
                Ok(socket) => Ok(socket),
                Err(_) => UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0))),
            }?
        };

        socket.set_read_timeout(Some(READ_TIMEOUT))?;

        Ok(Self {
            socket,
            next_tid: 0,
            read_only,
            request_timeout: request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT),
            inflight_requests: Vec::with_capacity(u16::MAX as usize),
        })
    }

    // === Options ===

    /// Returns the address the server is listening to.
    #[inline]
    pub fn local_addr(&self) -> Result<SocketAddr, std::io::Error> {
        self.socket.local_addr()
    }

    // === Public Methods ===

    /// Returns true if this message's transaction_id is still inflight
    pub fn inflight(&self, transaction_id: &u16) -> bool {
        self.inflight_requests
            .binary_search_by(|request| request.tid.cmp(transaction_id))
            .is_ok()
    }

    /// Send a request to the given address and return the transaction_id
    pub fn request(&mut self, address: SocketAddr, request: RequestSpecific) -> u16 {
        let message = self.request_message(request);

        self.inflight_requests.push(InflightRequest {
            tid: message.transaction_id,
            to: address,
            sent_at: Instant::now(),
        });

        let tid = message.transaction_id;
        let _ = self.send(address, message).map_err(|e| {
            debug!(?e, "Error sending request message");
        });

        tid
    }

    /// Send a response to the given address.
    pub fn response(
        &mut self,
        address: SocketAddr,
        transaction_id: u16,
        response: ResponseSpecific,
    ) {
        let message =
            self.response_message(MessageType::Response(response), address, transaction_id);
        let _ = self.send(address, message).map_err(|e| {
            debug!(?e, "Error sending response message");
        });
    }

    /// Send an error to the given address.
    pub fn error(&mut self, address: SocketAddr, transaction_id: u16, error: ErrorSpecific) {
        let message = self.response_message(MessageType::Error(error), address, transaction_id);
        let _ = self.send(address, message).map_err(|e| {
            debug!(?e, "Error sending error message");
        });
    }

    /// Receives a single krpc message on the socket.
    /// On success, returns the dht message and the origin.
    pub fn recv_from(&mut self) -> Option<(Message, SocketAddr)> {
        let mut buf = [0u8; MTU];

        // Cleanup timed-out transaction_ids.
        // Find the first timedout request, and delete all earlier requests.
        match self.inflight_requests.binary_search_by(|request| {
            if request.sent_at.elapsed() > self.request_timeout {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }) {
            Ok(index) => {
                self.inflight_requests.drain(..index);
            }
            Err(index) => {
                self.inflight_requests.drain(..index);
            }
        };

        if let Ok((amt, from)) = self.socket.recv_from(&mut buf) {
            let bytes = &buf[..amt];

            if from.port() == 0 {
                trace!("Response from port 0");
                return None;
            }

            match Message::from_bytes(bytes) {
                Ok(message) => {
                    // Parsed correctly.
                    match message.message_type {
                        // Positive or an error response or to an inflight request.
                        MessageType::Response(_) | MessageType::Error(_) => {
                            match self.inflight_requests.binary_search_by(|request| {
                                request.tid.cmp(&message.transaction_id)
                            }) {
                                Ok(index) => {
                                    let inflight_request = self
                                        .inflight_requests
                                        .get(index)
                                        .expect("should be infallible");

                                    if compare_socket_addr(&inflight_request.to, &from) {
                                        // Confirm that it is a response we actually sent.
                                        self.inflight_requests.remove(index);
                                    } else {
                                        trace!(?message, "Response from the wrong address");

                                        return None;
                                    }
                                }
                                Err(_) => {
                                    trace!(?message, "Unexpected response id");
                                }
                            }
                        }
                        _ => {}
                    };

                    return Some((message, from));
                }
                Err(error) => {
                    trace!(?error, ?bytes, "Received invalid message");
                }
            };
        };

        None
    }

    // === Private Methods ===

    /// Increments self.next_tid and returns the previous value.
    fn tid(&mut self) -> u16 {
        // We don't bother much with reusing freed transaction ids,
        // since the timeout is so short we are unlikely to run out
        // of 65535 ids in 2 seconds.
        let tid = self.next_tid;
        self.next_tid = self.next_tid.wrapping_add(1);
        tid
    }

    /// Set transactin_id, version and read_only
    fn request_message(&mut self, message: RequestSpecific) -> Message {
        let transaction_id = self.tid();

        Message {
            transaction_id,
            message_type: MessageType::Request(message),
            version: Some(VERSION.into()),
            read_only: self.read_only,
            requester_ip: None,
        }
    }

    /// Same as request_message but with request transaction_id and the requester_ip.
    fn response_message(
        &mut self,
        message: MessageType,
        requester_ip: SocketAddr,
        request_tid: u16,
    ) -> Message {
        Message {
            transaction_id: request_tid,
            message_type: message,
            version: Some(VERSION.into()),
            read_only: self.read_only,
            // BEP0042 Only relevant in responses.
            requester_ip: Some(requester_ip),
        }
    }

    /// Send a raw dht message
    fn send(&mut self, address: SocketAddr, message: Message) -> Result<(), SendMessageError> {
        trace!(?message, "Sending a message");
        self.socket.send_to(&message.to_bytes()?, address)?;
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

// Same as SocketAddr::eq but ingores the ip if it is unspecified for testing reasons.
fn compare_socket_addr(a: &SocketAddr, b: &SocketAddr) -> bool {
    if a.port() != b.port() {
        return false;
    }

    if a.ip().is_unspecified() {
        return true;
    }

    a.ip() == b.ip()
}

#[cfg(test)]
mod test {
    use std::thread;

    use crate::common::{Id, PingResponseArguments, RequestTypeSpecific};

    use super::*;

    #[test]
    fn tid() {
        let mut socket = KrpcSocket::new(false, None, None).unwrap();

        assert_eq!(socket.tid(), 0);
        assert_eq!(socket.tid(), 1);
        assert_eq!(socket.tid(), 2);

        socket.next_tid = u16::MAX;

        assert_eq!(socket.tid(), 65535);
        assert_eq!(socket.tid(), 0);
    }

    #[test]
    fn recv_request() {
        let mut server = KrpcSocket::new(false, None, None).unwrap();
        let server_address = server.local_addr().unwrap();

        let mut client = KrpcSocket::new(true, None, None).unwrap();
        client.next_tid = 120;

        let client_address = client.local_addr().unwrap();
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
                assert_eq!(
                    message.version,
                    Some(VERSION.to_vec()),
                    "Version should be 'RS'"
                );
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

        let mut client = KrpcSocket::new(true, None, None).unwrap();
        let client_address = client.local_addr().unwrap();

        let responder_id = Id::random();
        let response = ResponseSpecific::Ping(PingResponseArguments { responder_id });

        let server_thread = thread::spawn(move || {
            let mut server = KrpcSocket::new(true, None, None).unwrap();
            let server_address = server.local_addr().unwrap();
            tx.send(server_address).unwrap();

            loop {
                server.inflight_requests.push(InflightRequest {
                    tid: 8,
                    to: client_address,
                    sent_at: Instant::now(),
                });

                if let Some((message, from)) = server.recv_from() {
                    assert_eq!(from.port(), client_address.port());
                    assert_eq!(message.transaction_id, 8);
                    assert!(message.read_only, "Read-only should be true");
                    assert_eq!(
                        message.version,
                        Some(VERSION.to_vec()),
                        "Version should be 'RS'"
                    );
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
        let mut server = KrpcSocket::new(true, None, None).unwrap();
        let server_address = server.local_addr().unwrap();

        let mut client = KrpcSocket::new(true, None, None).unwrap();

        let client_address = client.local_addr().unwrap();

        server.inflight_requests.push(InflightRequest {
            tid: 8,
            to: SocketAddr::from(([127, 0, 0, 1], client_address.port() + 1)),
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
}
