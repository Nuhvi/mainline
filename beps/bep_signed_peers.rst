:BEP: ??
:Title: DHT Signed Peer Announcements
:Version: $Revision$
:Last-Modified: $Date$
:Author: Anonymous
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
peer discovery for protocols built on top of alternative transport
layers.


Rationale
=========

The current ``announce_peer`` mechanism in BEP 5 [#BEP-5]_ is designed
for peers reachable via traditional TCP/IP sockets. However, many
modern peer-to-peer applications operate over overlay networks where
peers are identified by cryptographic public keys rather than IP
addresses. This BEP provides a mechanism for peers to announce their
presence using signed public key identities while maintaining
compatibility with the existing DHT infrastructure.


Specification
=============

This BEP introduces two new DHT queries: ``announce_signed_peer`` and
``get_signed_peers``. These queries follow the same token-based
anti-spam mechanism as ``announce_peer`` and ``get_peers`` in BEP 5.
The signing mechanism and field names (``k`` and ``sig``) follow the
conventions established in BEP 44 [#BEP-44]_ for mutable items.

announce_signed_peer
--------------------

Announce that a peer with a given Ed25519 public key is participating
in a torrent. The query has six arguments:

- ``id``: 20-byte node ID of the querying node
- ``info_hash``: 20-byte infohash of the target torrent
- ``token``: opaque token received from a previous ``get_peers`` or
  ``get_signed_peers`` query to the same node
- ``k``: 32-byte Ed25519 public key representing the peer's identity
- ``sig``: 64-byte Ed25519 signature
- ``t``: 64-bit signed integer representing Unix time in microseconds

The ``k`` and ``sig`` fields use the same Ed25519 signing scheme as
BEP 44 [#BEP-44]_. The signature must be computed over the concatenation
of ``info_hash`` and ``t`` (28 bytes total). The queried node must verify:

1. The token was previously issued to the querying node's IP address
2. The timestamp ``t`` is within ±45 seconds of the current time
3. The signature is valid for the provided public key

If verification succeeds, the queried node stores the tuple
``(k, t, sig)`` associated with the infohash. If verification fails,
the node should respond with a protocol error (error code 203).

::

  arguments:  {"id" : "<querying nodes id>",
    "info_hash" : "<20-byte infohash of target torrent>",
    "token" : "<opaque token>",
    "k" : "<32-byte Ed25519 public key>",
    "sig" : "<64-byte Ed25519 signature>",
    "t" : <Unix time in microseconds>}

  response: {"id" : "<queried nodes id>"}

Example Packets:
::

  announce_signed_peer Query = {"t":"aa", "y":"q", "q":"announce_signed_peer", 
    "a": {"id":"abcdefghij0123456789", 
          "info_hash":"mnopqrstuvwxyz123456", 
          "token": "aoeusnth",
          "k": "0123456789abcdefghijklmnopqrstuv",
          "sig": "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ01",
          "t": 1729785600000000}}
  bencoded = d1:ad2:id20:abcdefghij01234567899:info_hash20:mnopqrstuvwxyz1234561:k32:0123456789abcdefghijklmnopqrstuv3:sig64:0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ011:ti1729785600000000e5:token8:aoeusnthe1:q18:announce_signed_peer1:t2:aa1:y1:qe

::

  Response = {"t":"aa", "y":"r", "r": {"id":"mnopqrstuvwxyz123456"}}
  bencoded = d1:rd2:id20:mnopqrstuvwxyz123456e1:t2:aa1:y1:re


get_signed_peers
----------------

Get signed peer announcements associated with a torrent infohash. The
query has two arguments, identical to ``get_peers``:

- ``id``: 20-byte node ID of the querying node  
- ``info_hash``: 20-byte infohash of the target torrent

If the queried node has signed peer announcements for the infohash,
they are returned in a key ``peers`` as a list of strings. Each string
is 104 bytes containing the compact signed peer info: 32-byte public
key, 8-byte timestamp (Unix time in microseconds as big-endian signed
integer), and 64-byte signature. The queried node should return a
random sample of stored announcements, similar to ``get_peers``
behavior for regular peers.

If the queried node has no signed peer announcements for the infohash,
it returns a key ``nodes`` containing the K closest nodes in its
routing table. In either case, a ``token`` key is included for use in
future ``announce_signed_peer`` queries.

::

  arguments:  {"id" : "<querying nodes id>", 
    "info_hash" : "<20-byte infohash of target torrent>"}

  response: {"id" : "<queried nodes id>", 
    "token" :"<opaque write token>", 
    "peers" : ["<104-byte signed peer info>", "<104-byte signed peer info>"]}

  or: {"id" : "<queried nodes id>", 
    "token" :"<opaque write token>", 
    "nodes" : "<compact node info>"}

Example Packets:
::

  get_signed_peers Query = {"t":"aa", "y":"q", "q":"get_signed_peers", 
    "a": {"id":"abcdefghij0123456789", 
          "info_hash":"mnopqrstuvwxyz123456"}}
  bencoded = d1:ad2:id20:abcdefghij01234567899:info_hash20:mnopqrstuvwxyz123456e1:q16:get_signed_peers1:t2:aa1:y1:qe

::

  Response with peers = {"t":"aa", "y":"r", "r": {"id":"abcdefghij0123456789", 
    "token":"aoeusnth", 
    "peers": ["<104 bytes>", "<104 bytes>"]}}
  bencoded = d1:rd2:id20:abcdefghij01234567895:peersl104:<binary>104:<binary>e5:token8:aoeusnthe1:t2:aa1:y1:re

::

  Response with closest nodes = {"t":"aa", "y":"r", "r": {"id":"abcdefghij0123456789", 
    "token":"aoeusnth", 
    "nodes": "def456..."}}
  bencoded = d1:rd2:id20:abcdefghij01234567895:nodes9:def456...5:token8:aoeusnthe1:t2:aa1:y1:re


Compact Signed Peer Info Encoding
----------------------------------

Signed peer information is encoded as a 104-byte string with the
following structure:

- Bytes 0-31: Ed25519 public key (32 bytes)
- Bytes 32-39: Unix timestamp in microseconds, big-endian signed 64-bit integer (8 bytes)
- Bytes 40-103: Ed25519 signature (64 bytes)

The signature is computed over the concatenation of the infohash (20
bytes) and timestamp (8 bytes).


Implementation Notes
====================

Bootstrap Strategy
------------------

During initial deployment when few nodes support this extension,
implementations should maintain a separate routing table for signed
peer announcements, similar to the approach used for IPv6 support in
BEP 32 [#BEP-32]_.

Implementations can identify supporting nodes by their version string
(the optional ``v`` key in KRPC messages). Nodes advertising version
strings known to support this BEP should be added to the signed peer
routing table.

The first implementation supporting this BEP uses version ``[82, 83, 0, 6]``
("RS" version 06) and is available at https://github.com/nuhvi/mainline.

Since Libtorrent and μTorrent implementations dominate the reachable
DHT nodes, coordination with these projects may be sufficient for
widespread adoption.

Storage Considerations
----------------------

Nodes should implement reasonable limits on stored signed peer
announcements per infohash to prevent resource exhaustion. Nodes may
expire stored announcements based on their timestamp or implement LRU
eviction policies.

When responding to ``get_signed_peers`` queries, nodes should return a
random sample of stored valid announcements rather than all stored
values, similar to the behavior specified for ``get_peers`` in BEP 5.

Overlay Network Namespacing
----------------------------

While not strictly part of this specification, implementations may wish
to distinguish between different overlay networks using the same
underlying topic or identifier. This can be accomplished by deriving
network-specific infohashes from a common topic identifier.

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
