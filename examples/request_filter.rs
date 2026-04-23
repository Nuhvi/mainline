use std::net::SocketAddrV4;

use n0_mainline::{Dht, RequestFilter, RequestSpecific, ServerSettings};

#[derive(Debug, Default, Clone)]
struct Filter;

impl RequestFilter for Filter {
    fn allow_request(&self, request: &RequestSpecific, from: SocketAddrV4) -> bool {
        tracing::info!(?request, ?from, "Got Request");

        true
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
        .build()?;

    client.bootstrapped().await?;

    let info = client.info().await?;

    println!("{:?}", info);

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}
