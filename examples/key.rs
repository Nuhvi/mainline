//! A command-line tool that creates Ed25519 key pairs for mutable items.
//!
//! ```sh
//! # Print a new random secret key in hex.
//! cargo run --example key -- new-key
//!
//! # Print the public key of a secret key in hex.
//! cargo run --example key -- get-public <64 hex characters secret key>
//! ```

use std::convert::TryFrom;

use clap::{Parser, Subcommand};
use dht::SigningKey;
use eyre::{eyre, Result};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate a new random secret key and print it in hex.
    NewKey,
    /// Print the public key of a secret key in hex.
    GetPublic {
        /// Secret key in hex.
        secret_key: String,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::NewKey => {
            let mut secret_key = [0; 32];

            getrandom::fill(&mut secret_key)
                .map_err(|_| eyre!("failed to generate random bytes"))?;

            println!("{}", to_hex(&secret_key));
        }
        Command::GetPublic { secret_key } => {
            let signer = from_hex(&secret_key)?;

            println!("{}", to_hex(signer.verifying_key().as_bytes()));
        }
    }

    Ok(())
}

fn from_hex(s: &str) -> Result<SigningKey> {
    if !s.len().is_multiple_of(2) {
        return Err(eyre!("Number of Hex characters should be even"));
    }

    let mut bytes = Vec::with_capacity(s.len() / 2);
    let mut digits = s.bytes().map(hex_digit);

    while let (Some(high), Some(low)) = (digits.next(), digits.next()) {
        bytes.push(high? << 4 | low?);
    }

    SigningKey::try_from(bytes.as_slice()).map_err(|_| eyre!("Invalid signing key"))
}

fn hex_digit(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(eyre!("Invalid hex character")),
    }
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    bytes.iter().fold(String::new(), |mut acc, byte| {
        let _ = write!(acc, "{byte:02x}");
        acc
    })
}
