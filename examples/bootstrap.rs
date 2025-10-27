use dht::Dht;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
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
