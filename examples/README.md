# Examples

## Core API Examples
These examples demonstrate the main functionality of the Mainline DHT library:

### Setup
```sh
# Bootstrap a DHT node
cargo run --example bootstrap

# Implement a custom request filter
cargo run --example request_filter

# Cache and reuse bootstrap nodes
cargo run --example cache_bootstrap

# Advanced logging configuration
cargo run --example logging
```

### Anounce/GET Peers (to announce Ip/port addresses)
```sh
# Announce as a peer
cargo run --example announce_peer <40 bytes hex info_hash>

# Find peers
cargo run --example get_peers <40 bytes hex info_hash>
```

### Anounce/GET Signed Peers (for overlay networks)
```sh
# Announce as a peer
cargo run --example announce_signed_peer <40 bytes hex info_hash> <64 bytes hex secret key>

# Find peers
cargo run --example get_peers <40 bytes hex info_hash>
```

### PUT/GET Arbitrary Immutable values.
```sh
# Store immutable data
cargo run --example put_immutable <string>

# Retrieve immutable data
cargo run --example get_immutable <40 bytes hex target from put_immutable>
```

### PUT/GET Arbitrary Mutable items.
```sh

# Store mutable data
cargo run --example put_mutable <64 bytes hex secret_key> <string>

# Retrieve mutable data
cargo run --example get_mutable <40 bytes hex target from put_mutable>
```

---

## Analysis & Research Tools
These examples are for DHT network analysis and research purposes:

> Note: These tools are not part of the main API and are provided for curiosity/research only.

```sh
# Analyze DHT node distribution
cargo run --example count_ips_close_to_key

# Estimate DHT size (Mark-Recapture method)
cargo run --example mark_recapture_dht

# Measure DHT network size
cargo run --example measure_dht
```
