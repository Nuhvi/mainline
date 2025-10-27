use std::{str::FromStr, time::Instant};

use dht::{Dht, Id, SigningKey};

use clap::Parser;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// info_hash to announce a peer on
    infohash: String,
    /// Mutable data public key.
    secret_key: String,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    // Usually you want to create the info hash from hashing the concatenation
    // of the topic you are interested in and a namespacing based on the overlay
    // network you are using, and any other diffrentiators to filter out peers
    // you can't or don't want to connect to by accident.
    let info_hash = Id::from_str(cli.infohash.as_str()).expect("invalid infohash");

    let dht = Dht::client().unwrap();

    let signer = from_hex(cli.secret_key);

    println!(
        "\nAnnouncing signed peer {} on an infohash: {} ...\n",
        to_hex(signer.verifying_key().as_bytes()),
        cli.infohash,
    );

    println!("\n=== COLD QUERY ===");
    announce(&dht, info_hash, &signer);

    println!("\n=== SUBSEQUENT QUERY ===");
    announce(&dht, info_hash, &signer);
}

fn announce(dht: &Dht, info_hash: Id, signer: &SigningKey) {
    let start = Instant::now();

    dht.announce_signed_peer(info_hash, signer)
        .expect("announce_peer failed");

    println!(
        "Announced peer in {:?} seconds",
        start.elapsed().as_secs_f32()
    );
}

fn from_hex(s: String) -> SigningKey {
    if s.len() % 2 != 0 {
        panic!("Number of Hex characters should be even");
    }

    let mut bytes = Vec::with_capacity(s.len() / 2);

    for i in 0..s.len() / 2 {
        let byte_str = &s[i * 2..(i * 2) + 2];
        let byte = u8::from_str_radix(byte_str, 16).expect("Invalid hex character");
        bytes.push(byte);
    }

    SigningKey::try_from(bytes.as_slice()).expect("Invalid signing key")
}

fn to_hex(bytes: &[u8]) -> String {
    let hex_chars: String = bytes.iter().map(|byte| format!("{:02x}", byte)).collect();

    hex_chars
}
