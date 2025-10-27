use std::time::Instant;

use dht::Dht;

use clap::Parser;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Value to store on the DHT
    value: String,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let dht = Dht::client().unwrap();
    let value = cli.value.as_bytes();

    println!("\nStoring immutable data: {} ...\n", cli.value);
    println!("\n=== COLD QUERY ===");
    put_immutable(&dht, &value);

    println!("\n=== SUBSEQUENT QUERY ===");
    put_immutable(&dht, &value);
}

fn put_immutable(dht: &Dht, value: &[u8]) {
    let start = Instant::now();

    let info_hash = dht.put_immutable(value).expect("put immutable failed");

    println!(
        "Stored immutable data as {:?} in {:?} milliseconds",
        info_hash,
        start.elapsed().as_millis()
    );
}
