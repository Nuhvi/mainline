use std::{str::FromStr, time::Instant};

use n0_mainline::{Dht, Id};

use clap::Parser;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Immutable data sha1 hash to lookup.
    target: String,
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

    let info_hash = Id::from_str(cli.target.as_str())?;

    let dht = Dht::client()?;

    println!("\nLooking up immutable data: {} ...\n", cli.target);

    println!("\n=== COLD QUERY ===");
    get_immutable(&dht, info_hash).await?;

    println!("\n=== SUBSEQUENT QUERY ===");
    get_immutable(&dht, info_hash).await?;

    Ok(())
}

async fn get_immutable(dht: &Dht, info_hash: Id) -> anyhow::Result<()> {
    let start = Instant::now();

    let value = dht
        .get_immutable(info_hash)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no immutable value found for the provided info_hash"))?;

    let string = String::from_utf8(value.to_vec())?;

    println!(
        "Got result in {:?} milliseconds\n",
        start.elapsed().as_millis()
    );

    println!("Got immutable data: {:?}", string);

    println!(
        "\nQuery exhausted in {:?} milliseconds",
        start.elapsed().as_millis(),
    );

    Ok(())
}
