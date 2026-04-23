use std::time::Instant;

use n0_mainline::{Dht, MutableItem, SigningKey};

use clap::Parser;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Mutable data public key.
    secret_key: String,
    /// Value to store on the DHT
    value: String,
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

    let dht = Dht::client()?;

    let secret_bytes: [u8; 32] = hex::decode(&cli.secret_key)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("secret key must be 32 bytes"))?;
    let signer = SigningKey::from_bytes(&secret_bytes);

    println!(
        "\nStoring mutable data: \"{}\" for public_key: {}  ...",
        cli.value,
        hex::encode(signer.verifying_key().as_bytes())
    );

    println!("\n=== COLD QUERY ===");
    put(&dht, &signer, cli.value.as_bytes(), None).await?;

    println!("\n=== SUBSEQUENT QUERY ===");
    put(&dht, &signer, cli.value.as_bytes(), None).await?;

    Ok(())
}

async fn put(
    dht: &Dht,
    signer: &SigningKey,
    value: &[u8],
    salt: Option<&[u8]>,
) -> anyhow::Result<()> {
    let start = Instant::now();

    let (item, cas) = if let Some(most_recent) = dht
        .get_mutable_most_recent(signer.verifying_key().as_bytes(), salt)
        .await?
    {
        let mut new_value = most_recent.value().to_vec();

        println!(
            "Found older value {:?}, appending new value to the old...",
            new_value
        );

        new_value.extend_from_slice(value);

        let most_recent_seq = most_recent.seq();
        let new_seq = most_recent_seq + 1;

        println!("Found older seq {most_recent_seq} incremnting sequence to {new_seq}...",);

        (
            MutableItem::new(signer, &new_value, new_seq, salt),
            Some(most_recent_seq),
        )
    } else {
        (MutableItem::new(signer, value, 1, salt), None)
    };

    dht.put_mutable(item, cas).await?;

    println!(
        "Stored mutable data as {:?} in {:?} milliseconds",
        hex::encode(signer.verifying_key().as_bytes()),
        start.elapsed().as_millis()
    );

    Ok(())
}
