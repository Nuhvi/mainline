use std::collections::HashSet;

use crate::{common::MAX_BUCKET_SIZE_K, Id, Node};

#[derive(Debug, Clone)]
/// Manage closest nodes found in a query.
///
/// Useful to estimate the Dht size.
pub struct ClosestNodes {
    target: Id,
    nodes: Vec<Node>,
}

impl ClosestNodes {
    /// Create a new instance of [`ClosestNodes`].
    #[must_use]
    pub fn new(target: Id) -> Self {
        Self {
            target,
            nodes: Vec::with_capacity(200),
        }
    }

    // === Getters ===

    /// Returns the target of the query for these closest nodes.
    #[must_use]
    pub fn target(&self) -> Id {
        self.target
    }

    /// Returns a slice of the nodes array.
    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Returns the number of nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns true if there are no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    // === Public Methods ===

    /// Add a node.
    pub fn add(&mut self, node: Node) {
        let seek = node.id().xor(&self.target);

        if node.already_exists(&self.nodes) {
            return;
        }

        if let Err(pos) = self.nodes.binary_search_by(|prope| {
            if prope.is_secure() && !node.is_secure() {
                std::cmp::Ordering::Less
            } else if !prope.is_secure() && node.is_secure() {
                std::cmp::Ordering::Greater
            } else if prope.id() == node.id() {
                std::cmp::Ordering::Equal
            } else {
                prope.id().xor(&self.target).cmp(&seek)
            }
        }) {
            self.nodes.insert(pos, node);
        }
    }

    /// Take enough nodes closest to the target, until the following are satisfied:
    /// 1. At least the closest `k` nodes (20).
    /// 2. The last node should be at a distance `edk` which is the expected distance of the 20th
    ///    node given previous estimations of the DHT size.
    /// 3. The number of subnets with unique 6 bits prefix in nodes ipv4 addresses match or exceeds
    ///    the average from previous queries.
    ///
    /// If one or more of these conditions are not met, then we just take all responding nodes
    /// and store data at them.
    #[must_use]
    pub fn take_until_secure(
        &self,
        previous_dht_size_estimate: usize,
        average_subnets: usize,
    ) -> &[Node] {
        let mut until_secure = 0;

        // 20 / dht_size_estimate == expected_dk / ID space
        // so expected_dk = 20 * ID space / dht_size_estimate
        // Computed in u128 integer arithmetic to avoid float precision loss.
        let denominator = previous_dht_size_estimate as u128 + 1;
        let expected_dk = if denominator <= 20 {
            // Saturating the following equation
            u128::MAX
        } else {
            20 * (u128::MAX / denominator) + (20 * (u128::MAX % denominator)) / denominator
        };

        let mut subnets = HashSet::new();

        for node in &self.nodes {
            let distance = distance(&self.target, node);

            subnets.insert(subnet(node));

            if distance >= expected_dk && subnets.len() >= average_subnets {
                break;
            }

            until_secure += 1;
        }

        let end = until_secure.max(MAX_BUCKET_SIZE_K).min(self.nodes().len());

        self.nodes.get(0..end).unwrap_or(&[])
    }

    /// Count the number of subnets with unique 6 bits prefix in ipv4
    #[must_use]
    pub fn subnets_count(&self) -> u8 {
        if self.nodes.is_empty() {
            return 20;
        }

        let mut subnets = HashSet::new();

        for node in self.nodes.iter().take(MAX_BUCKET_SIZE_K) {
            subnets.insert(subnet(node));
        }

        subnets.len().try_into().unwrap_or_default()
    }

    /// An estimation of the Dht from the distribution of closest nodes
    /// responding to a query.
    ///
    /// [Read more](https://github.com/nuhvi/mainline/blob/main/docs/dht_size_estimate.md)
    #[must_use]
    pub fn dht_size_estimate(&self) -> f64 {
        dht_size_estimate(
            self.nodes
                .iter()
                .take(MAX_BUCKET_SIZE_K)
                .map(|node| distance(&self.target, node)),
        )
    }
}

fn subnet(node: &Node) -> u8 {
    ((node.address().ip().to_bits() >> 26) & 0b0011_1111) as u8
}

fn distance(target: &Id, node: &Node) -> u128 {
    let xor = node.id().xor(target);

    // Round up the lower 4 bytes to get a u128 from u160.
    let low = xor.as_bytes().split_at(16).0;

    u128::from_be_bytes(low.try_into().unwrap_or_default())
}

fn dht_size_estimate<I>(distances: I) -> f64
where
    I: IntoIterator<Item = u128>,
{
    let mut sum = 0.0;
    let mut count: u32 = 0;

    // Ignoring the first node, as that gives the best result in simulations.
    for distance in distances {
        count += 1;

        sum += f64::from(count) * distance as f64;
    }

    if count == 0 {
        return 0.0;
    }

    let lsq_constant = f64::from(count * (count + 1) * (2 * count + 1) / 6);

    lsq_constant * u128::MAX as f64 / sum
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, net::SocketAddrV4, str::FromStr, sync::Arc, time::Instant};

    use eyre::Result;

    use crate::common::NodeInner;

    use super::*;

    #[test]
    fn add_sorted_by_id() -> Result<()> {
        let target = Id::random();

        let mut closest_nodes = ClosestNodes::new(target);

        for i in 0..100 {
            let node = Node::unique(i);
            closest_nodes.add(node.clone());
            closest_nodes.add(node);
        }

        assert_eq!(closest_nodes.nodes().len(), 100);

        let distances = closest_nodes
            .nodes()
            .iter()
            .map(|n| n.id().distance(&target))
            .collect::<Vec<_>>();

        let mut sorted = distances.clone();
        sorted.sort_unstable();

        assert_eq!(sorted, distances);
        Ok(())
    }

    #[test]
    fn order_by_secure_id() -> Result<()> {
        let unsecure = Node::random();
        let secure = Node(Arc::new(NodeInner {
            id: Id::from_str("5a3ce9c14e7a08645677bbd1cfe7d8f956d53256")?,
            address: SocketAddrV4::new([21, 75, 31, 124].into(), 0),
            token: None,
            last_seen: Instant::now(),
        }));

        let mut closest_nodes = ClosestNodes::new(*unsecure.id());

        closest_nodes.add(unsecure.clone());
        closest_nodes.add(secure.clone());

        assert_eq!(closest_nodes.nodes(), vec![secure, unsecure]);
        Ok(())
    }

    #[test]
    fn take_until_expected_distance_to_20th_node() -> Result<()> {
        let target = Id::random();
        let dht_size_estimate = 200;

        let mut closest_nodes = ClosestNodes::new(target);

        let target_bytes = target.as_bytes();

        for i in 0..dht_size_estimate {
            let node = Node::unique(i);
            closest_nodes.add(node);
        }

        let mut sybil = ClosestNodes::new(target);

        for _ in 0..20 {
            let mut bytes = target_bytes.to_vec();
            let (_, tail) = bytes.split_at_mut(18);
            let random_id = Id::random();
            tail.copy_from_slice(random_id.as_bytes().split_at(18).1);
            let node = Node::new(Id::random(), SocketAddrV4::new(0.into(), 0));

            sybil.add(node.clone());
            closest_nodes.add(node);
        }

        let closest = closest_nodes.take_until_secure(dht_size_estimate, 0);

        assert!((closest.len() - sybil.nodes().len()) > 10);
        Ok(())
    }

    #[test]
    fn simulation() -> Result<()> {
        let lookups = 4;
        let acceptable_margin = 0.2;
        let sims = 10;
        let dht_size = 2500_f64;

        let mean = (0..sims)
            .map(|_| simulate(dht_size as usize, lookups) as f64)
            .sum::<f64>()
            / f64::from(sims);

        let margin = (mean - dht_size).abs() / dht_size;

        assert!(margin <= acceptable_margin);
        Ok(())
    }

    fn simulate(dht_size: usize, lookups: usize) -> usize {
        let mut nodes = BTreeMap::new();
        for i in 0..dht_size {
            let node = Node::unique(i);
            nodes.insert(*node.id(), node);
        }

        (0..lookups)
            .map(|_| {
                let target = Id::random();

                let mut closest_nodes = ClosestNodes::new(target);

                for (_, node) in nodes.range(target..).take(100) {
                    closest_nodes.add(node.clone());
                }
                for (_, node) in nodes.range(..target).rev().take(100) {
                    closest_nodes.add(node.clone());
                }

                let estimate = closest_nodes.dht_size_estimate();

                estimate as usize
            })
            .sum::<usize>()
            / lookups
    }
}
