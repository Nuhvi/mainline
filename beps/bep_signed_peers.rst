:BEP: ??
:Title: DHT Signed Peer Announcements
:Version: $Revision$
:Last-Modified: $Date$
:Author: Nuh <nuh@nuh.dev>
:Status: Draft
:Type: Standards Track
:Created: 24-Oct-2025
:Post-History:

Abstract
========

This BEP extends the DHT protocol to support announcing and discovering
cryptographically signed peer identities. Instead of announcing IP
address and port combinations, peers can announce Ed25519 public keys
that represent addresses on overlay networks. This enables trackerless
peer discovery for protocols built on top of alternative overaly networks.


Rationale
=========

The current ``announce_peer`` mechanism in BEP_0005 [#BEP-5]_ is designed
for peers reachable via traditional TCP/IP sockets. However, many
modern peer-to-peer applications operate over overlay networks where
peers are identified by cryptographic public keys (mainly Ed25519 keys) 
rather than IP addresses. This BEP provides a mechanism for peers to announce 
their presence using signed public key identities while maintaining
compatibility with the existing DHT infrastructure.


Specification
=============

This BEP introduces two new DHT queries: ``announce_signed_peer`` and
``get_signed_peers``. These queries follow the same token-based
anti-spam mechanism as ``announce_peer`` and ``get_peers`` in BEP_0005 [#BEP-5]_.
The signing mechanism and field names (``k`` and ``sig``) follow the
conventions established in BEP_0044 [#BEP-44]_ for mutable items.

announce_signed_peer
--------------------

Announce a peer with a given Ed25519 public key over some info hash. 
The query has six arguments:

- ``id``: 20-byte node ID of the querying node
- ``info_hash``: 20-byte infohash of the target torrent
- ``token``: opaque token received from a previous ``get_peers`` or ``get_signed_peers`` query to the same node
- ``k``: 32-byte Ed25519 public key representing the peer's identity
- ``sig``: 64-byte Ed25519 signature
- ``t``: 64-bit signed integer representing Unix time in microseconds

The signature must be computed over the concatenation of the ``info_hash`` (20 bytes) and ``t`` (big-endian 64-bit integer 8 bytes). 

Other than the token validation inherited from BEP_0005 [#BEP-5]_, the queried node must verify:

1. The timestamp ``t`` is within ±45 seconds of the current time
2. The signature is valid for the provided public key

If verification succeeds, the queried node stores the tuple ``(k, t, sig)`` associated with the infohash. 
If verification fails, the node should respond with a protocol error (error code 203) describing the failure.

::

  arguments:  {"id" : "<querying nodes id>",
    "info_hash" : "<20-byte infohash of target torrent>",
    "token" : "<opaque token>",
    "k" : "<32-byte Ed25519 public key>",
    "sig" : "<64-byte Ed25519 signature>",
    "t" : <Unix time in microseconds>}

  response: {"id" : "<queried nodes id>"}

get_signed_peers
----------------

Get signed peer announcements associated with a torrent infohash. The
query has two arguments, identical to ``get_peers``:

- ``id``: 20-byte node ID of the querying node  
- ``info_hash``: 20-byte infohash of the target torrent

The queried node should return a random sample of stored announcements, 
similar to ``get_peers`` behavior for regular peers lookup.

If the queried node has signed peer announcements for the infohash,
they are returned in a key ``peers`` as a list of strings, 
each is encoded as a 104 bytes string with the following structure:

- (32 bytes): Ed25519 public key
- (8 bytes): Unix timestamp in microseconds, big-endian 64-bit integer
- (64 bytes): Ed25519 signature

The signature is computed over the concatenation of the infohash (20bytes) and timestamp (8 bytes) as described above.

If the queried node has no signed peer announcements for the infohash,
it should omit the ``peers`` key.

Whether the node has peers or not, it should return a key ``nodes`` containing the K closest nodes in its routing table. 
A ``token`` key should also be included for use in future ``announce_signed_peer`` queries, whether or not the node has signed ``peers``.

::

  arguments:  {"id" : "<querying nodes id>", 
    "info_hash" : "<20-byte infohash of target torrent>"}

  response: {"id" : "<queried nodes id>", 
    "token" :"<opaque write token>", 
    "nodes" : "<compact node info>",
    "peers" : ["<104-byte signed peer info>", "<104-byte signed peer info>"]}

  or: {"id" : "<queried nodes id>", 
    "token" :"<opaque write token>", 
    "nodes" : "<compact node info>"}

Implementation Notes
====================

Bootstrap Strategy
------------------

During initial deployment when few nodes support this extension, 
it will be very unlikely for the closest nodes in the normal routing table
to be supporting this new api. So implementations should maintain a separate 
routing table for nodes known to support signed peer announcements and lookup, 
similar to the approach used for IPv6 support in BEP_0032 [#BEP-32]_.

To enable that filtering, implementations can depend on the version string
(the optional ``v`` key in KRPC messages). Nodes advertising version
strings known to support this BEP should be added to the signed peers routing table.

For example, knowing that version ``[82, 83, 0, 6]`` supports this api,
means that any node with ``v`` starts with ``[82, 83]`` and ends with ``[0, 6]`` or
higher, should be added to the signed peers routing table.

The first implementation supporting this BEP uses version ``[82, 83, 0, 6]``
("RS" version 06) and is available at https://github.com/nuhvi/mainline.

A bootstrapping node supporting this proposal is running at ``relay.pkarr.org:6881``.

Overlay Network Namespacing
----------------------------

Implementations may wish to distinguish between different overlay networks using the same underlying topic or identifier. 
This can be accomplished by deriving network-specific infohashes from a common topic identifier.

For example, an overlay network like Iroh [#Iroh]_ wanting to announce
peers for a topic identified by a BLAKE3 hash could derive an infohash
by hashing the concatenation of the topic hash and a network identifier
string (e.g., "Iroh"), using BLAKE3 again then taking the first 20 bytes. 
Alternatively, SHA-1 could be applied to the same concatenation. 

Different overlay networks can announce peers for the same underlying topic 
by using different network identifier strings in the derivation, ensuring
namespace separation while sharing the same routing resources.

Security Considerations
-----------------------

The 45-second timestamp window provides reasonable clock skew tolerance
while limiting the potential for replay attacks. Implementations should
reject announcements with timestamps outside this window.

Nodes must verify Ed25519 signatures before storing announcements to
prevent malicious peers from announcing arbitrary public keys.


References
==========

.. [#BEP-5] BEP_0005. DHT Protocol.
   (http://www.bittorrent.org/beps/bep_0005.html)

.. [#BEP-32] BEP_0032. IPv6 extension for DHT.
   (http://www.bittorrent.org/beps/bep_0032.html)

.. [#BEP-44] BEP_0044. Storing arbitrary data in the DHT.
   (http://www.bittorrent.org/beps/bep_0044.html)

.. [#Iroh] Iroh. A toolkit for building distributed applications.
   (https://www.iroh.computer/)


Copyright
=========

This document has been placed in the public domain.
