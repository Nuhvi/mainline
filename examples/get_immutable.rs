//! A command-line tool that looks up immutable data on the DHT by info-hash.

use std::{str::FromStr, time::Instant};

use clap::Parser;
use dht::{Dht, Id};
use eyre::{OptionExt, Result, WrapErr};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Immutable data sha1 hash to lookup.
    target: String,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let info_hash = Id::from_str(cli.target.as_str())?;

    let dht = Dht::client()?;

    println!("\nLooking up immutable data: {} ...\n", cli.target);

    println!("\n=== COLD QUERY ===");
    get_immutable(&dht, info_hash)?;

    println!("\n=== SUBSEQUENT QUERY ===");
    get_immutable(&dht, info_hash)?;

    Ok(())
}

fn get_immutable(dht: &Dht, info_hash: Id) -> Result<()> {
    let start = Instant::now();

    // No need to stream responses, just print the first result, since
    // all immutable data items are guaranteed to be the same.
    let value = dht
        .get_immutable(info_hash)
        .ok_or_eyre("Failed to find the immutable value for the provided info_hash")?;

    let string = String::from_utf8(value.to_vec())
        .wrap_err("expected immutable data to be valid utf-8 for this demo")?;

    println!(
        "Got result in {:?} milliseconds\n",
        start.elapsed().as_millis()
    );

    println!("Got immutable data: {string:?}");

    println!(
        "\nQuery exhausted in {:?} milliseconds",
        start.elapsed().as_millis(),
    );

    Ok(())
}
