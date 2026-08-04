//! A command-line tool that announces a peer on the DHT network for a given info-hash.
//!
//! The program accepts an infohash as a command-line argument, initialises a DHT client,
//! and performs two announcements (cold query and subsequent query) to measure the time taken.

use std::{str::FromStr, time::Instant};

use clap::Parser;
use dht::{Dht, Id};
use eyre::Result;

/// Command-line arguments for the announcement tool.
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// The 20‑byte SHA‑1 info_hash of the torrent to announce a peer on.
    /// It can be passed as a hexadecimal string or as a URL‑encoded string.
    infohash: String,
}

/// Entry point of the program.
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    // Parse the infohash from the command line.
    let info_hash = Id::from_str(cli.infohash.as_str())?;

    // Initialise the DHT client.
    let dht = Dht::client()?;

    println!("\nAnnouncing peer on an infohash: {} ...\n", cli.infohash);

    // Perform the first announcement (cold query – no cached routing table).
    println!("\n=== COLD QUERY ===");
    announce(&dht, info_hash)?;

    // Perform a second announcement (warm query – uses cached information).
    println!("\n=== SUBSEQUENT QUERY ===");
    announce(&dht, info_hash)?;

    Ok(())
}

/// Announces a peer on the DHT for a given info‑hash.
fn announce(dht: &Dht, info_hash: Id) -> Result<()> {
    let start = Instant::now();

    // Announce the peer; `None` for the port would use the default,
    // but we explicitly supply `Some(6991)` as requested.
    dht.announce_peer(info_hash, Some(6991))?;

    println!(
        "Announced peer in {:?} seconds",
        start.elapsed().as_secs_f32()
    );

    Ok(())
}
