use std::time::Instant;

use mainline::Dht;

use clap::Parser;

use tracing::Level;
use tracing_subscriber;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Value to store on the DHT
    value: String,
}

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .init();

    let cli = Cli::parse();

    let dht = Dht::default();

    let start = Instant::now();

    println!("\nStoring immutable data: {} ...\n", cli.value);

    let metadata = dht
        .put_immutable(cli.value.into())
        .expect("put immutable failed");
    println!(
        "Stored immutable data as {:?} in {:?} seconds",
        metadata.target(),
        start.elapsed().as_secs_f32()
    );
    let stored_at = metadata.stored_at();
    println!("Stored at: {:?} nodes", stored_at.len());
    for node in stored_at {
        println!("   {:?}", node);
    }

    // You can now republish to the same closest nodes
    // skipping the the lookup step.
    //
    // This time we choose to not sepcify the port, effectively
    // making the port implicit to be detected by the storing node
    // from the source address of the put_immutable request
    //
    // Uncomment the following lines to try it out:

    // println!(
    //     "Publishing immutable data again to {:?} closest_nodes ...",
    //     metadata.closest_nodes().len()
    // );
    //
    // let again = Instant::now();
    // match dht.put_immutable_to(target, value, metadata.closest_nodes()) {
    //     Ok(metadata) => {
    //         println!(
    //             "Published again to {:?} nodes in {:?} seconds",
    //             metadata.stored_at().len(),
    //             again.elapsed().as_secs()
    //         );
    //     }
    //     Err(err) => {
    //         println!("Error: {:?}", err);
    //     }
    // }
}
