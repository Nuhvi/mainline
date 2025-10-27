use std::net::SocketAddrV4;

use dht::{Dht, RequestFilter, RequestSpecific, ServerSettings};

#[derive(Debug, Default, Clone)]
struct Filter;

impl RequestFilter for Filter {
    fn allow_request(&self, request: &RequestSpecific, from: SocketAddrV4) -> bool {
        tracing::info!(?request, ?from, "Got Request");

        true
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let client = Dht::builder()
        .server_mode()
        .server_settings(ServerSettings {
            filter: Box::new(Filter),
            ..Default::default()
        })
        .build()
        .unwrap();

    client.bootstrapped();

    let info = client.info();

    println!("{:?}", info);

    loop {}
}
