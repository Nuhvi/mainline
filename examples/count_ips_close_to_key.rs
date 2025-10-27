//!
//! * Counts all IP addresses around a random target ID and counts the number of hits, each IP gets.
//! * Does this by initializing a new DHT node for each lookups to reach the target from different directions.
//! *
//! * The result shows how sloppy the lookup algorithms are.
//! *
//! Prints a histogram with the collected nodes
//! first column are the buckets indicating the hit rate. 3 .. 12 summerizes the nodes that get hit with a probability of 3 to 12% in each lookup.
//! Second column indicates the number of nodes that this bucket contains. [19] means 19 nodes got hit with a probability of 3 to 12%.
//! Third column is a visualization of the number of nodes [19].
//!
//! Example1:
//! 3 .. 12 [ 19 ]: ∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎
//! Within one lookup, 19 nodes got hit in 3 to 12% of the cases. These are rarely found therefore.
//!
//! Example2:
//! 84 .. 93 [ 15 ]: ∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎
//! Within one lookup, 15 nodes got hit in 84 to 93% of the cases. These nodes are therefore found in almost all lookups.
//!
//! Full example:
//! 3 .. 12 [ 19 ]: ∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎
//! 12 .. 21 [  2 ]: ∎∎
//! 21 .. 30 [  3 ]: ∎∎∎
//! 30 .. 39 [  2 ]: ∎∎
//! 39 .. 48 [  3 ]: ∎∎∎
//! 48 .. 57 [  0 ]:
//! 57 .. 66 [  0 ]:
//! 66 .. 75 [  0 ]:
//! 75 .. 84 [  1 ]: ∎
//! 84 .. 93 [ 15 ]: ∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎
//!

use dht::{Dht, Id, Node};
use histo::Histogram;
use std::{
    collections::{HashMap, HashSet},
    net::Ipv4Addr,
    sync::mpsc::channel,
};

const K: usize = 20; // Not really k but we take the k closest nodes into account.
const MAX_DISTANCE: u8 = 150; // Health check to not include outrageously distant nodes.
const USE_RANDOM_BOOTSTRAP_NODES: bool = false;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let target = Id::random();
    let mut ip_hits: HashMap<Ipv4Addr, u16> = HashMap::new();
    let (tx_interrupted, rx_interrupted) = channel();

    println!("Count all IP addresses around a random target_key={target} k={K} max_distance={MAX_DISTANCE} random_boostrap={USE_RANDOM_BOOTSTRAP_NODES}.");
    println!("Press CTRL+C to show the histogram");
    println!();

    ctrlc::set_handler(move || {
        println!();
        println!("Received Ctrl+C! Finishing current lookup. Hold on...");
        tx_interrupted.send(()).unwrap();
    })
    .expect("Error setting Ctrl-C handler");

    let mut last_nodes: HashSet<Ipv4Addr> = HashSet::new();
    let mut lookup_count = 0;
    while rx_interrupted.try_recv().is_err() {
        lookup_count += 1;
        let dht = init_dht(USE_RANDOM_BOOTSTRAP_NODES);
        let nodes = dht.find_node(target);
        let nodes: Box<[Node]> = nodes
            .iter()
            .filter(|node| target.distance(node.id()) < MAX_DISTANCE)
            .cloned()
            .collect();
        let closest_nodes = nodes.iter().take(K).cloned().collect::<Box<[_]>>();
        let sockets: HashSet<Ipv4Addr> = closest_nodes
            .iter()
            .map(|node| *node.address().ip())
            .collect();
        for socket in sockets.iter() {
            let previous = ip_hits.get(socket);
            match previous {
                Some(val) => {
                    ip_hits.insert(socket.clone(), val + 1);
                }
                None => {
                    ip_hits.insert(socket.clone(), 1);
                }
            };
        }

        if closest_nodes.is_empty() {
            continue;
        }
        let closest_node = closest_nodes.first().unwrap();
        let closest_distance = target.distance(closest_node.id());
        let furthest_node = closest_nodes.last().unwrap();
        let furthest_distance = target.distance(furthest_node.id());

        let overlap_with_last_lookup: HashSet<Ipv4Addr> =
            sockets.intersection(&last_nodes).map(|ip| *ip).collect();

        let overlap = overlap_with_last_lookup.len() as f64 / K as f64;
        last_nodes = sockets;
        println!(
            "lookup={:02} Ips found {}. Closest node distance: {}, furthest node distance: {}, overlap with previous lookup {}%",
            lookup_count,
            ip_hits.len(),
            closest_distance,
            furthest_distance,
            (overlap*100 as f64) as usize
        );
    }

    println!();
    println!("Histogram");
    print_histogram(ip_hits, lookup_count);
}

fn print_histogram(hits: HashMap<Ipv4Addr, u16>, lookup_count: usize) {
    /*

    */
    let mut histogram = Histogram::with_buckets(10);
    let percents: HashMap<Ipv4Addr, u64> = hits
        .into_iter()
        .map(|(ip, hits)| {
            let percent = (hits as f32 / lookup_count as f32) * 100 as f32;
            (ip, percent as u64)
        })
        .collect();

    for (_, percent) in percents.iter() {
        histogram.add(percent.clone());
    }

    println!("{}", histogram);
}

fn get_random_boostrap_nodes2() -> Vec<String> {
    let dht = Dht::client().unwrap();
    let nodes = dht.find_node(Id::random());
    let addrs = nodes
        .iter()
        .map(|node| node.address().to_string())
        .collect::<Box<[_]>>();
    let slice: Vec<String> = addrs[..8].into_iter().map(|va| va.clone()).collect();
    slice
}

fn init_dht(use_random_boostrap_nodes: bool) -> Dht {
    if use_random_boostrap_nodes {
        let bootstrap = get_random_boostrap_nodes2();
        return Dht::builder().bootstrap(&bootstrap).build().unwrap();
    } else {
        Dht::client().unwrap()
    }
}
