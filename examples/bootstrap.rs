//! Bootstraps a local DHT server and re-bootstraps a client from it.

use dht::Dht;
use eyre::Result;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let client = Dht::server()?;

    client.bootstrapped();

    let info = client.info();

    println!("{info:?}");

    let client = Dht::builder()
        .bootstrap(&[client.info().local_addr()])
        .build()?;

    client.bootstrapped();

    let info = client.info();

    println!("Bootstrapped using local node. {info:?}");

    Ok(())
}
