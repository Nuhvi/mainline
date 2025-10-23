use dht::Dht;

use tracing::Level;
use tracing_subscriber;

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .init();

    let client = Dht::server().unwrap();

    client.bootstrapped();

    let info = client.info();

    println!("{info:?}");

    let client = Dht::builder()
        .bootstrap(&[info.local_addr()])
        .build()
        .unwrap();

    client.bootstrapped();

    let info = client.info();

    println!("Bootstrapped using local node. {info:?}");
}
