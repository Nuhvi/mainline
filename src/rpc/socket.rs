//! Udp socket layer managine incoming/outgoing requests and responses.

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};
use tracing::{debug, trace};

use crate::messages::{ErrorSpecific, Message, MessageType, RequestSpecific, ResponseSpecific};

use crate::Result;

const DEFAULT_PORT: u16 = 6881;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_millis(2000); // 2 seconds
const VERSION: [u8; 4] = [82, 83, 0, 1]; // "RS" version 01
const MTU: usize = 2048;
const READ_TIMEOUT: Duration = Duration::from_millis(100);

/// A UdpSocket wrapper that formats and correlates DHT requests and responses.
#[derive(Debug)]
pub struct KrpcSocket {
    next_tid: u16,
    socket: UdpSocket,
    pub read_only: bool,
    pub request_timeout: Duration,
    pub inflight_requests: HashMap<u16, InflightRequest>,
}

#[derive(Debug)]
pub struct InflightRequest {
    to: SocketAddr,
    sent_at: Instant,
}

impl KrpcSocket {
    pub fn new() -> Result<Self> {
        let socket = match UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT))) {
            Ok(socket) => Ok(socket),
            Err(_) => UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0))),
        }?;
        socket.set_read_timeout(Some(READ_TIMEOUT))?;

        Ok(Self {
            socket,
            next_tid: 0,
            read_only: false,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            inflight_requests: HashMap::new(),
        })
    }

    pub fn bind(port: u16) -> Result<Self> {
        let socket = UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], port)))?;
        socket.set_read_timeout(Some(READ_TIMEOUT))?;

        Ok(Self {
            socket,
            next_tid: 0,
            read_only: false,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            inflight_requests: HashMap::new(),
        })
    }

    // === Options ===

    /// Set read-only mode
    #[cfg(test)]
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Returns the address the server is listening to.
    #[inline]
    pub fn local_addr(&self) -> SocketAddr {
        self.socket.local_addr().unwrap()
    }

    // === Public Methods ===

    /// Send a request to the given address and return a receiver for the response.
    pub fn request(&mut self, address: SocketAddr, request: RequestSpecific) -> u16 {
        let message = self.request_message(request);

        self.inflight_requests.insert(
            message.transaction_id,
            InflightRequest {
                to: address,
                sent_at: Instant::now(),
            },
        );

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
            debug!(?e, "Error sending error");
        });
    }

    /// Receives a single krpc message on the socket.
    /// On success, returns the dht message and the origin.
    pub fn recv_from(&mut self) -> Option<(Message, SocketAddr)> {
        let mut buf = [0u8; MTU];

        // Cleanup timedout transaction_ids.
        let request_timeout = self.request_timeout;
        self.inflight_requests
            .retain(|_, request| request.sent_at.elapsed() < request_timeout);

        if let Ok((amt, from)) = self.socket.recv_from(&mut buf) {
            let bytes = &buf[..amt];

            match Message::from_bytes(bytes) {
                Ok(message) => {
                    // Parsed correctly.
                    match message.message_type {
                        MessageType::Request(_) => {
                            if self.read_only {
                                return None;
                            }

                            return Some((message, from));
                        }
                        // Response or an error to an inflight request.
                        _ => {
                            if let Some(inflight_request) =
                                self.inflight_requests.get(&message.transaction_id)
                            {
                                if compare_socket_addr(&inflight_request.to, &from) {
                                    // Confirm that it is a response we actually sent.
                                    self.inflight_requests.remove(&message.transaction_id);
                                    return Some((message, from));
                                } else {
                                    debug!("Response from the wrong address");
                                }
                            } else {
                                debug!("Unexpected response id");
                            };
                        }
                    }
                }
                Err(error) => {
                    debug!(?error, ?bytes, "Received invalid message");
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
    fn send(&mut self, address: SocketAddr, message: Message) -> Result<()> {
        trace!(?message, "Sending a message");
        self.socket.send_to(&message.to_bytes()?, address)?;
        Ok(())
    }
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

    use crate::{
        common::Id,
        messages::{PingRequestArguments, PingResponseArguments},
    };

    use super::*;

    #[test]
    fn tid() {
        let mut socket = KrpcSocket::new().unwrap();

        assert_eq!(socket.tid(), 0);
        assert_eq!(socket.tid(), 1);
        assert_eq!(socket.tid(), 2);

        socket.next_tid = u16::max_value();

        assert_eq!(socket.tid(), 65535);
        assert_eq!(socket.tid(), 0);
    }

    #[test]
    fn recv_request() {
        let mut server = KrpcSocket::new().unwrap();
        let server_address = server.local_addr();

        let mut client = KrpcSocket::new().unwrap().with_read_only(true);
        client.next_tid = 120;

        let client_address = client.local_addr();
        let request = RequestSpecific::Ping(PingRequestArguments {
            requester_id: Id::random(),
        });

        let expected_request = request.clone();

        let server_thread = thread::spawn(move || loop {
            if let Some((message, from)) = server.recv_from() {
                assert_eq!(from.port(), client_address.port());
                assert_eq!(message.transaction_id, 120);
                assert_eq!(message.read_only, true, "Read-only should be true");
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
        let mut server = KrpcSocket::new().unwrap();
        let server_address = server.local_addr();

        let mut client = KrpcSocket::new().unwrap().with_read_only(true);

        let client_address = client.local_addr();

        server.inflight_requests.insert(
            8,
            InflightRequest {
                to: client_address,
                sent_at: Instant::now(),
            },
        );

        let response = ResponseSpecific::Ping(PingResponseArguments {
            responder_id: Id::random(),
        });

        let expected_response = response.clone();

        let server_thread = thread::spawn(move || loop {
            if let Some((message, from)) = server.recv_from() {
                assert_eq!(from.port(), client_address.port());
                assert_eq!(message.transaction_id, 8);
                assert_eq!(message.read_only, true, "Read-only should be true");
                assert_eq!(
                    message.version,
                    Some(VERSION.to_vec()),
                    "Version should be 'RS'"
                );
                assert_eq!(
                    message.message_type,
                    MessageType::Response(expected_response)
                );
                break;
            }
        });

        client.response(server_address, 8, response);

        server_thread.join().unwrap();
    }

    #[test]
    fn ignore_unexcpected_response() {
        let mut server = KrpcSocket::new().unwrap();
        let server_address = server.local_addr();

        let mut client = KrpcSocket::new().unwrap().with_read_only(true);

        let _ = client.local_addr();

        let response = ResponseSpecific::Ping(PingResponseArguments {
            responder_id: Id::random(),
        });

        let _ = response.clone();

        let server_thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(5));
            assert!(
                server.recv_from().is_none(),
                "Should not receive a unexpected response"
            );
        });

        client.response(server_address, 120, response);

        server_thread.join().unwrap();
    }

    #[test]
    fn ignore_response_from_wrong_address() {
        let mut server = KrpcSocket::new().unwrap();
        let server_address = server.local_addr();

        let mut client = KrpcSocket::new().unwrap().with_read_only(true);

        let client_address = client.local_addr();

        server.inflight_requests.insert(
            8,
            InflightRequest {
                to: SocketAddr::from(([127, 0, 0, 1], client_address.port() + 1)),
                sent_at: Instant::now(),
            },
        );

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

        client.response(server_address, 120, response);

        server_thread.join().unwrap();
    }

    #[test]
    fn ignore_request_in_read_only() {
        let mut server = KrpcSocket::new().unwrap().with_read_only(true);
        let server_address = server.local_addr();

        let mut client = KrpcSocket::new().unwrap();
        client.next_tid = 120;

        let _ = client.local_addr();
        let request = RequestSpecific::Ping(PingRequestArguments {
            requester_id: Id::random(),
        });

        let _ = request.clone();

        let server_thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(5));
            assert!(
                server.recv_from().is_none(),
                "should not receieve requests in read-only mode"
            );
        });

        client.request(server_address, request);

        server_thread.join().unwrap();
    }
}
