use std::{
    collections::HashSet,
    str::FromStr,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use dht::{Dht, Id};

use clap::Parser;

use tracing::Level;
use tracing_subscriber;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// info_hash to lookup peers for
    infohash: String,
}

fn main() {
    tracing_subscriber::fmt()
        // Switch to DEBUG to see incoming values and the IP of the responding nodes
        .with_max_level(Level::INFO)
        .init();

    let cli = Cli::parse();

    let info_hash = Id::from_str(cli.infohash.as_str()).expect("Expected info_hash");

    // let dht = Dht::client().unwrap();
    let dht = Dht::client().unwrap();

    dht.bootstrapped();

    println!("Looking up peers for info_hash: {} ...", info_hash);
    println!("\n=== COLD QUERY ===");
    get_peers(&dht, &info_hash);

    println!("\n=== SUBSEQUENT QUERY ===");
    println!("Looking up peers for info_hash: {} ...", info_hash);
    get_peers(&dht, &info_hash);
}

fn get_peers(dht: &Dht, info_hash: &Id) {
    let start = Instant::now();
    let mut first = false;

    let mut peers = HashSet::new();

    for response in dht.get_signed_peers(*info_hash) {
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
}
fn to_hex(bytes: &[u8]) -> String {
    let hex_chars: String = bytes.iter().map(|byte| format!("{:02x}", byte)).collect();

    hex_chars
}

fn time_ago(timestamp_micros: u64) -> String {
    // Get current time in microseconds since UNIX epoch
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_micros() as u64;

    // Calculate difference in microseconds
    let diff_micros = now.saturating_sub(timestamp_micros);

    // Convert to seconds
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
            let months = seconds / 2592000; // ~30 days
            format!("{} month{} ago", months, if months == 1 { "" } else { "s" })
        }
        _ => {
            let years = seconds / 31536000; // ~365 days
            format!("{} year{} ago", years, if years == 1 { "" } else { "s" })
        }
    }
}
