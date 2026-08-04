//! A command-line tool that looks up mutable data on the DHT by public key.

use std::convert::TryFrom;
use std::time::Instant;

use clap::Parser;
use dht::{Dht, MutableItem};
use ed25519_dalek::VerifyingKey;
use eyre::{eyre, Result};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Mutable data public key.
    public_key: String,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let public_key = from_hex(&cli.public_key)?.to_bytes();
    let dht = Dht::client()?;

    println!("Looking up mutable item: {} ...", cli.public_key);

    println!("\n=== COLD LOOKUP ===");
    get_first(&dht, &public_key);

    println!("\n=== SUBSEQUENT LOOKUP ===");
    get_first(&dht, &public_key);

    println!("\n=== GET MOST RECENT ===");
    let start = Instant::now();

    println!("\nLooking up the most recent value..");
    let item = dht.get_mutable_most_recent(&public_key, None);

    if let Some(item) = item {
        println!("Found the most recent value:");
        print_value(&item);
    } else {
        println!("Not found");
    }

    println!(
        "\nQuery exhausted in {:?} seconds.",
        start.elapsed().as_secs_f32(),
    );

    Ok(())
}

fn get_first(dht: &Dht, public_key: &[u8; 32]) {
    let start = Instant::now();
    if let Some(item) = dht.get_mutable(public_key, None, None).next() {
        println!(
            "\nGot first result in {:?} milliseconds:",
            start.elapsed().as_millis()
        );
        print_value(&item);
    } else {
        println!("Not Found")
    }
}

fn print_value(item: &MutableItem) {
    match String::from_utf8(item.value().to_vec()) {
        Ok(string) => {
            println!("  mutable item: {:?}, seq: {:?}", string, item.seq());
        }
        Err(_) => {
            println!("  mutable item: {:?}, seq: {:?}", item.value(), item.seq(),);
        }
    };
}

fn from_hex(s: &str) -> Result<VerifyingKey> {
    if s.len() % 2 != 0 {
        return Err(eyre!("Number of Hex characters should be even"));
    }

    let mut bytes = Vec::with_capacity(s.len() / 2);

    for i in 0..s.len() / 2 {
        let byte_str = &s[i * 2..(i * 2) + 2];
        let byte = u8::from_str_radix(byte_str, 16).map_err(|_| eyre!("Invalid hex character"))?;
        bytes.push(byte);
    }

    VerifyingKey::try_from(bytes.as_slice()).map_err(|_| eyre!("Invalid mutable key"))
}
