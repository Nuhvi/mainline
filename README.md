# Mainline

Simple, robust, `BitTorrent`'s [Mainline DHT](https://en.wikipedia.org/wiki/Mainline_DHT) implementation.

This library is focused on being the best and simplest Rust client for Mainline, especially focused on reliable and fast time-to-first-response.

It should work as a routing / storing node (server mode) as well, and has been running in production for many months without an issue.
However if you are concerned about spam or `DoS`, you should consider implementing [rate limiting](#rate-limiting).

**[API Docs](https://docs.rs/dht/latest/dht/)**

## Getting started

Check the [Examples](https://github.com/nuhvi/mainline/tree/main/examples).

## Features

### Client

Running as a client, means you can store and query for values on the DHT, but not accept any incoming requests.

```rust
use dht::Dht;

let dht = Dht::client().unwrap();
```

Supported BEPs:
- [x] [BEP_0005 DHT Protocol](https://www.bittorrent.org/beps/bep_0005.html)
- [x] [BEP_0042 DHT Security extension](https://www.bittorrent.org/beps/bep_0042.html)
- [x] [BEP_0043 Read-only DHT Nodes](https://www.bittorrent.org/beps/bep_0043.html)
- [x] [BEP_0044 Storing arbitrary data in the DHT](https://www.bittorrent.org/beps/bep_0044.html)
- [x] [BEP_???? Announce and lookup signed peers](https://github.com/bittorrent/bittorrent.org/pull/174).

This implementation also includes [measures against Vertical Sybil Attacks](https://github.com/Nuhvi/mainline/blob/main/docs/censorship-resistance.md#vertical-sybil-attacks).

### Server

Running as a server is the same as a client, but you also respond to incoming requests and serve as a routing and storing node, supporting the general routing of the DHT, and contributing to the storage capacity of the DHT.

```rust
use dht::Dht;

let dht = Dht::server().unwrap(); // or `Dht::builder().server_mode().build();`
```

Supported BEPs:
- [x] [BEP_0005 DHT Protocol](https://www.bittorrent.org/beps/bep_0005.html)
- [x] [BEP_0042 DHT Security extension](https://www.bittorrent.org/beps/bep_0042.html)
- [x] [BEP_0043 Read-only DHT Nodes](https://www.bittorrent.org/beps/bep_0043.html)
- [x] [BEP_0044 Storing arbitrary data in the DHT](https://www.bittorrent.org/beps/bep_0044.html)
- [x] [BEP_???? Announce and lookup signed peers](https://github.com/bittorrent/bittorrent.org/pull/174).

#### Rate limiting

The server implementation has no rate-limiting, you can run your own [request filter](./examples/request_filter.rs) and apply your custom rate-limiting.
However, that limit/block will only apply _after_ parsing incoming messages, and it won't affect handling incoming responses.

### Adaptive mode

The default Adaptive mode starts the node in client mode. After 15 minutes, it switches to server mode if a self-ping confirms the public address.
This way nodes that can serve as routing nodes (accessible and less likely to churn) serve as such.

The self-ping is a best-effort heuristic. NAT hairpinning can give a false positive. A blocked hairpin path can give a false negative.
A false positive mainly costs other nodes: they may add the node to their routing tables, send probes, and evict it after their stale timeout.
The node itself may receive no inbound traffic if it is unreachable. A false positive cannot forge writes, because the write token binds to the source IP and target.

If you port-forward the node or run a public service such as iroh-dns-server, start in Server mode instead of relying on adaptive mode.
Call `Dht::builder().server_mode().build()`. You can optionally add your known public IP so the node does not depend on votes from responding nodes:
`Dht::builder().server_mode().public_ip(ip).build()`, where `ip` is your `Ipv4Addr`.

## Acknowledgment

This implementation was possible thanks to [Webtorrent's Bittorrent-dht](https://github.com/webtorrent/bittorrent-dht) as a reference,
and [Rustydht-lib](https://github.com/raptorswing/rustydht-lib) that saved me a lot of time, especially at the serialization and deserialization of Bencode messages.
