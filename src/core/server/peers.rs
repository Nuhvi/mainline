//! Manage announced peers for info_hashes

use std::{net::SocketAddrV4, num::NonZeroUsize};

use crate::common::Id;

use lru::LruCache;

const CHANCE_SCALE: f32 = 2.0 * (1u32 << 31) as f32;

#[derive(Debug, Clone)]
/// An LRU cache of "Peers" per info hashes.
///
/// Read [BEP_0005](https://www.bittorrent.org/beps/bep_0005.html) for more information.
pub struct PeersStore {
    info_hashes: LruCache<Id, LruCache<Id, SocketAddrV4>>,
    max_peers: NonZeroUsize,
}

impl PeersStore {
    /// Create a new store of peers announced on info hashes.
    pub fn new(max_info_hashes: NonZeroUsize, max_peers: NonZeroUsize) -> Self {
        Self {
            info_hashes: LruCache::new(max_info_hashes),
            max_peers,
        }
    }

    /// Add a peer for an info hash.
    pub fn add_peer(&mut self, info_hash: Id, peer: (&Id, SocketAddrV4)) {
        if let Some(info_hash_lru) = self.info_hashes.get_mut(&info_hash) {
            info_hash_lru.put(*peer.0, peer.1);
        } else {
            let mut info_hash_lru = LruCache::new(self.max_peers);
            info_hash_lru.put(*peer.0, peer.1);
            self.info_hashes.put(info_hash, info_hash_lru);
        };
    }

    /// Returns a random set of peers per an info hash.
    pub fn get_random_peers(&mut self, info_hash: &Id) -> Option<Vec<SocketAddrV4>> {
        if let Some(info_hash_lru) = self.info_hashes.get(info_hash) {
            let size = info_hash_lru.len();
            let target_size = 20;

            if size == 0 {
                return None;
            }
            if size < target_size {
                return Some(
                    info_hash_lru
                        .iter()
                        .map(|n| n.1.to_owned())
                        .collect::<Vec<_>>(),
                );
            }

            let mut results = Vec::with_capacity(20);

            let mut chunk = vec![0_u8; info_hash_lru.iter().len() * 4];
            crate::common::fill_random(chunk.as_mut_slice());

            for (index, (_, addr)) in info_hash_lru.iter().enumerate() {
                // Calculate the chance of adding the current item based on remaining items and slots
                let remaining_slots = target_size - results.len();
                let remaining_items = info_hash_lru.len() - index;
                let current_chance =
                    ((remaining_slots as f32 / remaining_items as f32) * CHANCE_SCALE) as u32;

                // Get random integer from the chunk
                let rand_int = u32::from_le_bytes(
                    chunk
                        .get(index..index + 4)
                        .and_then(|s| s.try_into().ok())
                        .unwrap_or_default(),
                );

                // Randomly decide to add the item based on the current chance
                if rand_int < current_chance {
                    results.push(*addr);
                    if results.len() == target_size {
                        break;
                    }
                }
            }

            return Some(results);
        }

        None
    }
}

#[cfg(test)]
mod test {
    use eyre::{OptionExt, Result};

    use super::*;

    #[test]
    fn max_info_hashes() -> Result<()> {
        let mut store = PeersStore::new(
            NonZeroUsize::new(1).ok_or_eyre("non-zero value")?,
            NonZeroUsize::new(100).ok_or_eyre("non-zero value")?,
        );

        let info_hash_a = Id::random();
        let info_hash_b = Id::random();

        store.add_peer(
            info_hash_a,
            (&info_hash_a, SocketAddrV4::new([127, 0, 1, 1].into(), 0)),
        );
        store.add_peer(
            info_hash_b,
            (&info_hash_b, SocketAddrV4::new([127, 0, 1, 1].into(), 0)),
        );

        assert_eq!(store.info_hashes.len(), 1);
        assert_eq!(
            store.get_random_peers(&info_hash_b),
            Some([SocketAddrV4::new([127, 0, 1, 1].into(), 0)].into())
        );
        Ok(())
    }

    #[test]
    fn all_peers() -> Result<()> {
        let mut store = PeersStore::new(
            NonZeroUsize::new(1).ok_or_eyre("non-zero value")?,
            NonZeroUsize::new(2).ok_or_eyre("non-zero value")?,
        );

        let info_hash = Id::random();

        store.add_peer(
            info_hash,
            (&Id::random(), SocketAddrV4::new([127, 0, 1, 1].into(), 0)),
        );
        store.add_peer(
            info_hash,
            (&Id::random(), SocketAddrV4::new([127, 0, 1, 2].into(), 0)),
        );
        store.add_peer(
            info_hash,
            (&Id::random(), SocketAddrV4::new([127, 0, 1, 3].into(), 0)),
        );

        assert_eq!(
            store.get_random_peers(&info_hash),
            Some(
                [
                    SocketAddrV4::new([127, 0, 1, 3].into(), 0),
                    SocketAddrV4::new([127, 0, 1, 2].into(), 0),
                ]
                .into()
            )
        );
        Ok(())
    }

    #[test]
    fn random_peers_subset() -> Result<()> {
        let mut store = PeersStore::new(
            NonZeroUsize::new(1).ok_or_eyre("non-zero value")?,
            NonZeroUsize::new(200).ok_or_eyre("non-zero value")?,
        );

        let info_hash = Id::random();

        for i in 0..200 {
            store.add_peer(
                info_hash,
                (&Id::random(), SocketAddrV4::new([127, 0, 1, i].into(), 0)),
            )
        }

        assert_eq!(
            store
                .info_hashes
                .get(&info_hash)
                .ok_or_eyre("info hash not found")?
                .len(),
            200
        );

        let sample = store
            .get_random_peers(&info_hash)
            .ok_or_eyre("expected peers")?;

        assert_eq!(sample.len(), 20);
        Ok(())
    }
}
