use std::{str::FromStr, time::Instant};

use n0_mainline::{Dht, Id};

use clap::Parser;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// info_hash to announce a peer on
    infohash: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let info_hash = Id::from_str(cli.infohash.as_str())?;

    let dht = Dht::client()?;

    println!("\nAnnouncing peer on an infohash: {} ...\n", cli.infohash);

    println!("\n=== COLD QUERY ===");
    announce(&dht, info_hash).await?;

    println!("\n=== SUBSEQUENT QUERY ===");
    announce(&dht, info_hash).await?;

    Ok(())
}

async fn announce(dht: &Dht, info_hash: Id) -> anyhow::Result<()> {
    let start = Instant::now();

    dht.announce_peer(info_hash, Some(6991)).await?;

    println!(
        "Announced peer in {:?} seconds",
        start.elapsed().as_secs_f32()
    );

    Ok(())
}
