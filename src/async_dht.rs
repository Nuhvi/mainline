//! AsyncDht node.

use bytes::Bytes;
use std::net::SocketAddr;

use crate::{
    common::{
        hash_immutable, AnnouncePeerRequestArguments, FindNodeRequestArguments,
        GetPeersRequestArguments, GetValueRequestArguments, Id, MutableItem, Node,
        PutImmutableRequestArguments, PutMutableRequestArguments, PutRequestSpecific,
        RequestTypeSpecific,
    },
    dht::{ActorMessage, Dht, DhtPutError, DhtWasShutdown, Info},
    rpc::{PutError, ResponseSender},
};

impl Dht {
    /// Return an async version of the Dht client.
    pub fn as_async(self) -> AsyncDht {
        AsyncDht(self)
    }
}

#[derive(Debug, Clone)]
/// Async version of the Dht node.
pub struct AsyncDht(Dht);

impl AsyncDht {
    // === Getters ===

    /// Information and statistics about this [Dht] node.
    pub async fn info(&self) -> Result<Info, DhtWasShutdown> {
        let (sender, receiver) = flume::bounded::<Info>(1);

        self.0
             .0
            .send(ActorMessage::Info(sender))
            .map_err(|_| DhtWasShutdown)?;

        receiver.recv_async().await.map_err(|_| DhtWasShutdown)
    }

    // === Public Methods ===

    /// Shutdown the actor thread loop.
    pub async fn shutdown(&mut self) {
        let (sender, receiver) = flume::bounded::<()>(1);

        let _ = self.0 .0.send(ActorMessage::Shutdown(sender));
        let _ = receiver.recv_async().await;
    }

    // === Find nodes ===

    pub async fn find_node(&self, target: Id) -> Result<Vec<Node>, DhtWasShutdown> {
        let (sender, receiver) = flume::bounded::<Vec<Node>>(1);

        let request = RequestTypeSpecific::FindNode(FindNodeRequestArguments { target });

        self.0
             .0
            .send(ActorMessage::Get(
                target,
                request,
                ResponseSender::ClosestNodes(sender),
            ))
            .map_err(|_| DhtWasShutdown)?;

        Ok(receiver
            .recv_async()
            .await
            .expect("Query was dropped before sending a response, please open an issue."))
    }

    // === Peers ===

    /// Get peers for a given infohash.
    ///
    /// Note: each node of the network will only return a _random_ subset (usually 20)
    /// of the total peers it has for a given infohash, so if you are getting responses
    /// from 20 nodes, you can expect up to 400 peers in total, but if there are more
    /// announced peers on that infohash, you are likely to miss some, the logic here
    /// for Bittorrent is that any peer will introduce you to more peers through "peer exchange"
    /// so if you are implementing something different from Bittorrent, you might want
    /// to implement your own logic for gossipping more peers after you discover the first ones.
    pub fn get_peers(
        &self,
        info_hash: Id,
    ) -> Result<flume::r#async::RecvStream<Vec<SocketAddr>>, DhtWasShutdown> {
        // Get requests use unbounded channels to avoid blocking in the run loop.
        // Other requests like put_* and getters don't need that and is ok with
        // bounded channel with 1 capacity since it only ever sends one message back.
        //
        // So, if it is a ResponseMessage<_>, it should be unbounded, otherwise bounded.
        let (sender, receiver) = flume::unbounded::<Vec<SocketAddr>>();

        let request = RequestTypeSpecific::GetPeers(GetPeersRequestArguments { info_hash });

        self.0
             .0
            .send(ActorMessage::Get(
                info_hash,
                request,
                ResponseSender::Peers(sender),
            ))
            .map_err(|_| DhtWasShutdown)?;

        Ok(receiver.into_stream())
    }

    /// Announce a peer for a given infohash.
    ///
    /// The peer will be announced on this process IP.
    /// If explicit port is passed, it will be used, otherwise the port will be implicitly
    /// assumed by remote nodes to be the same ase port they recieved the request from.
    pub async fn announce_peer(&self, info_hash: Id, port: Option<u16>) -> Result<Id, DhtPutError> {
        let (sender, receiver) = flume::bounded::<Result<Id, PutError>>(1);

        let (port, implied_port) = match port {
            Some(port) => (port, None),
            None => (0, Some(true)),
        };

        let request = PutRequestSpecific::AnnouncePeer(AnnouncePeerRequestArguments {
            info_hash,
            port,
            implied_port,
        });

        self.0
             .0
            .send(ActorMessage::Put(info_hash, request, sender))
            .map_err(|_| DhtWasShutdown)?;

        Ok(receiver
            .recv_async()
            .await
            .expect("Query was dropped before sending a response, please open an issue.")?)
    }

    // === Immutable data ===

    /// Get an Immutable data by its sha1 hash.
    pub async fn get_immutable(&self, target: Id) -> Result<Option<Bytes>, DhtWasShutdown> {
        let (sender, receiver) = flume::unbounded::<Bytes>();

        let request = RequestTypeSpecific::GetValue(GetValueRequestArguments {
            target,
            seq: None,
            salt: None,
        });

        self.0
             .0
            .send(ActorMessage::Get(
                target,
                request,
                ResponseSender::Immutable(sender),
            ))
            .map_err(|_| DhtWasShutdown)?;

        Ok(receiver.recv_async().await.map(Some).unwrap_or(None))
    }

    /// Put an immutable data to the DHT.
    pub async fn put_immutable(&self, value: Bytes) -> Result<Id, DhtPutError> {
        let target: Id = hash_immutable(&value).into();

        let (sender, receiver) = flume::bounded::<Result<Id, PutError>>(1);

        let request = PutRequestSpecific::PutImmutable(PutImmutableRequestArguments {
            target,
            v: value.clone().into(),
        });

        self.0
             .0
            .send(ActorMessage::Put(target, request, sender))
            .map_err(|_| DhtWasShutdown)?;

        Ok(receiver
            .recv_async()
            .await
            .expect("Query was dropped before sending a response, please open an issue.")?)
    }

    // === Mutable data ===

    /// Get a mutable data by its public_key and optional salt.
    pub fn get_mutable(
        &self,
        public_key: &[u8; 32],
        salt: Option<Bytes>,
        seq: Option<i64>,
    ) -> Result<flume::r#async::RecvStream<MutableItem>, DhtWasShutdown> {
        let target = MutableItem::target_from_key(public_key, &salt);

        let (sender, receiver) = flume::unbounded::<MutableItem>();

        let request = RequestTypeSpecific::GetValue(GetValueRequestArguments { target, seq, salt });

        self.0
             .0
            .send(ActorMessage::Get(
                target,
                request,
                ResponseSender::Mutable(sender),
            ))
            .map_err(|_| DhtWasShutdown)?;

        Ok(receiver.into_stream())
    }

    /// Put a mutable data to the DHT.
    pub async fn put_mutable(&self, item: MutableItem) -> Result<Id, DhtPutError> {
        let (sender, receiver) = flume::bounded::<Result<Id, PutError>>(1);

        let request = PutRequestSpecific::PutMutable(PutMutableRequestArguments {
            target: *item.target(),
            v: item.value().clone().into(),
            k: item.key().to_vec(),
            seq: *item.seq(),
            sig: item.signature().to_vec(),
            salt: item.salt().clone().map(|s| s.to_vec()),
            cas: *item.cas(),
        });

        self.0
             .0
            .send(ActorMessage::Put(*item.target(), request, sender))
            .map_err(|_| DhtWasShutdown)?;

        Ok(receiver
            .recv_async()
            .await
            .expect("Query was dropped before sending a response, please open an issue.")?)
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use ed25519_dalek::SigningKey;
    use futures::StreamExt;

    use crate::dht::Testnet;

    use super::*;

    #[test]
    fn shutdown() {
        async fn test() {
            let mut dht = Dht::client().unwrap().as_async();

            let info = dht.info().await.unwrap();

            info.local_addr().unwrap();

            let a = dht.clone();

            dht.shutdown().await;

            let result = a.get_immutable(Id::random()).await;

            assert!(matches!(result, Err(DhtWasShutdown)))
        }
        futures::executor::block_on(test());
    }

    #[test]
    fn announce_get_peer() {
        async fn test() {
            let testnet = Testnet::new(10).unwrap();

            let a = Dht::builder()
                .bootstrap(&testnet.bootstrap)
                .build()
                .unwrap()
                .as_async();
            let b = Dht::builder()
                .bootstrap(&testnet.bootstrap)
                .build()
                .unwrap()
                .as_async();

            let info_hash = Id::random();

            a.announce_peer(info_hash, Some(45555))
                .await
                .expect("failed to announce");

            let peers = b
                .get_peers(info_hash)
                .unwrap()
                .next()
                .await
                .expect("No peers");

            assert_eq!(peers.first().unwrap().port(), 45555);
        }

        futures::executor::block_on(test());
    }

    #[test]
    fn put_get_immutable() {
        async fn test() {
            let testnet = Testnet::new(10).unwrap();

            let a = Dht::builder()
                .bootstrap(&testnet.bootstrap)
                .build()
                .unwrap()
                .as_async();
            let b = Dht::builder()
                .bootstrap(&testnet.bootstrap)
                .build()
                .unwrap()
                .as_async();

            let value: Bytes = "Hello World!".into();
            let expected_target = Id::from_str("e5f96f6f38320f0f33959cb4d3d656452117aadb").unwrap();

            let target = a.put_immutable(value.clone()).await.unwrap();
            assert_eq!(target, expected_target);

            let response = b.get_immutable(target).await.unwrap();
            assert_eq!(response, Some(value));
        }

        futures::executor::block_on(test());
    }

    #[test]
    fn put_get_mutable() {
        async fn test() {
            let testnet = Testnet::new(10).unwrap();

            let a = Dht::builder()
                .bootstrap(&testnet.bootstrap)
                .build()
                .unwrap()
                .as_async();
            let b = Dht::builder()
                .bootstrap(&testnet.bootstrap)
                .build()
                .unwrap()
                .as_async();

            let signer = SigningKey::from_bytes(&[
                56, 171, 62, 85, 105, 58, 155, 209, 189, 8, 59, 109, 137, 84, 84, 201, 221, 115, 7,
                228, 127, 70, 4, 204, 182, 64, 77, 98, 92, 215, 27, 103,
            ]);

            let seq = 1000;
            let value: Bytes = "Hello World!".into();

            let item = MutableItem::new(signer.clone(), value, seq, None);

            a.put_mutable(item.clone()).await.unwrap();

            let response = b
                .get_mutable(signer.verifying_key().as_bytes(), None, None)
                .unwrap()
                .next()
                .await
                .expect("No mutable values");

            assert_eq!(&response, &item);
        }

        futures::executor::block_on(test());
    }

    #[test]
    fn put_get_mutable_no_more_recent_value() {
        async fn test() {
            let testnet = Testnet::new(10).unwrap();

            let a = Dht::builder()
                .bootstrap(&testnet.bootstrap)
                .build()
                .unwrap()
                .as_async();
            let b = Dht::builder()
                .bootstrap(&testnet.bootstrap)
                .build()
                .unwrap()
                .as_async();

            let signer = SigningKey::from_bytes(&[
                56, 171, 62, 85, 105, 58, 155, 209, 189, 8, 59, 109, 137, 84, 84, 201, 221, 115, 7,
                228, 127, 70, 4, 204, 182, 64, 77, 98, 92, 215, 27, 103,
            ]);

            let seq = 1000;
            let value: Bytes = "Hello World!".into();

            let item = MutableItem::new(signer.clone(), value, seq, None);

            a.put_mutable(item.clone()).await.unwrap();

            let response = b
                .get_mutable(signer.verifying_key().as_bytes(), None, Some(seq))
                .unwrap()
                .next()
                .await;

            assert!(&response.is_none());
        }

        futures::executor::block_on(test());
    }

    #[test]
    fn repeated_put_query() {
        async fn test() {
            let testnet = Testnet::new(10).unwrap();

            let a = Dht::builder()
                .bootstrap(&testnet.bootstrap)
                .build()
                .unwrap()
                .as_async();

            let first = a.put_immutable(vec![1, 2, 3].into());
            let second = a.put_immutable(vec![1, 2, 3].into());

            assert_eq!(first.await.unwrap(), second.await.unwrap());
        }

        futures::executor::block_on(test());
    }
}
