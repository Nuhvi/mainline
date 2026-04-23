use std::{
    collections::HashSet,
    str::FromStr,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use futures::StreamExt;
use n0_mainline::{Dht, Id};

use clap::Parser;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// info_hash to lookup peers for
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

    dht.bootstrapped().await?;

    println!("Looking up peers for info_hash: {} ...", info_hash);
    println!("\n=== COLD QUERY ===");
    get_peers(&dht, &info_hash).await?;

    println!("\n=== SUBSEQUENT QUERY ===");
    println!("Looking up peers for info_hash: {} ...", info_hash);
    get_peers(&dht, &info_hash).await?;

    Ok(())
}

async fn get_peers(dht: &Dht, info_hash: &Id) -> anyhow::Result<()> {
    let start = Instant::now();
    let mut first = false;

    let mut peers = HashSet::new();

    let mut stream = dht.get_signed_peers(*info_hash).await?;
    while let Some(response) = stream.next().await {
        if !first {
            first = true;
            println!(
                "Got first result in {:?} milliseconds:",
                start.elapsed().as_millis()
            );

            println!(
                "peers {:?}",
                response
                    .iter()
                    .map(|p| (to_hex(p.key()), time_ago(p.timestamp())))
                    .collect::<Vec<_>>()
            );
        }

        for peer in response {
            peers.insert(peer);
        }
    }

    println!(
        "\nQuery exhausted in {:?} milliseconds, got {:?} unique peers.",
        start.elapsed().as_millis(),
        peers.len()
    );

    Ok(())
}
fn to_hex(bytes: &[u8]) -> String {
    let hex_chars: String = bytes.iter().map(|byte| format!("{:02x}", byte)).collect();

    hex_chars
}

fn time_ago(timestamp_micros: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_micros() as u64;

    let diff_micros = now.saturating_sub(timestamp_micros);
    let seconds = diff_micros / 1_000_000;

    match seconds {
        0..=59 => format!(
            "{} second{} ago",
            seconds,
            if seconds == 1 { "" } else { "s" }
        ),
        60..=3599 => {
            let minutes = seconds / 60;
            format!(
                "{} minute{} ago",
                minutes,
                if minutes == 1 { "" } else { "s" }
            )
        }
        3600..=86399 => {
            let hours = seconds / 3600;
            format!("{} hour{} ago", hours, if hours == 1 { "" } else { "s" })
        }
        86400..=2591999 => {
            let days = seconds / 86400;
            format!("{} day{} ago", days, if days == 1 { "" } else { "s" })
        }
        2592000..=31535999 => {
            let months = seconds / 2592000;
            format!("{} month{} ago", months, if months == 1 { "" } else { "s" })
        }
        _ => {
            let years = seconds / 31536000;
            format!("{} year{} ago", years, if years == 1 { "" } else { "s" })
        }
    }
}
