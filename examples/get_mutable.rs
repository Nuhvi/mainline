use ed25519_dalek::VerifyingKey;
use std::convert::TryFrom;

use std::time::Instant;

use futures::StreamExt;
use n0_mainline::{Dht, MutableItem};

use clap::Parser;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Mutable data public key.
    public_key: String,
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

    let public_key = from_hex(cli.public_key.clone())?.to_bytes();
    let dht = Dht::client()?;

    println!("Looking up mutable item: {} ...", cli.public_key);

    println!("\n=== COLD LOOKUP ===");
    get_first(&dht, &public_key).await?;

    println!("\n=== SUBSEQUENT LOOKUP ===");
    get_first(&dht, &public_key).await?;

    println!("\n=== GET MOST RECENT ===");
    let start = Instant::now();

    println!("\nLooking up the most recent value..");
    let item = dht.get_mutable_most_recent(&public_key, None).await?;

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

async fn get_first(dht: &Dht, public_key: &[u8; 32]) -> anyhow::Result<()> {
    let start = Instant::now();
    if let Some(item) = dht.get_mutable(public_key, None, None).await?.next().await {
        println!(
            "\nGot first result in {:?} milliseconds:",
            start.elapsed().as_millis()
        );
        print_value(&item);
    } else {
        println!("Not Found")
    }
    Ok(())
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

fn from_hex(s: String) -> anyhow::Result<VerifyingKey> {
    let bytes = hex::decode(&s)?;
    Ok(VerifyingKey::try_from(bytes.as_slice())?)
}
