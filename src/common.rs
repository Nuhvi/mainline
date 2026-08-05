//! Miscellaneous common structs used throughout the library.

mod closest_nodes;
mod id;
mod immutable;
pub(crate) mod messages;
mod mutable;
mod node;
mod routing_table;
mod signed_announce;

pub use closest_nodes::*;
pub use id::*;
pub(crate) use immutable::*;
pub use messages::*;
pub use mutable::*;
pub use node::*;
pub use routing_table::*;
pub(crate) use signed_announce::*;

/// Fill `bytes` with cryptographically random data.
///
/// Panics if the operating system random number generator fails,
/// which is not expected to happen on supported platforms.
#[allow(clippy::expect_used)]
pub(crate) fn fill_random(bytes: &mut [u8]) {
    getrandom::fill(bytes).expect("OS random number generator failed")
}
