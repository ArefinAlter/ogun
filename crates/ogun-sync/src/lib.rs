//! Edge-to-hub sync for Ogun (spec O-11; ADR 0008).
//!
//! An edge (a Raspberry Pi or site PC running `ogund` and `ogun-ingest`) seals stream windows into
//! content-addressed batches. This crate moves them to a hub over links that fail:
//!
//! * [`keys`]: the edge's Ed25519 identity, generated once, stored in a `0600` file, zeroized in
//!   memory, enrolled on the hub by a person (`edges.enroll`).
//! * [`signing`]: the signing rule for offers: Ed25519 over a context string and the RFC 8785 (JCS)
//!   form of the offer, the same canonicalisation `ogun-ir` uses for workflow hashes.
//! * [`edge`]: the edge-side client. It finds sealed window revisions on the edge's own `ogund`,
//!   keeps a durable outbox, sends signed offers and the wanted chunks to the hub's sync bridge
//!   over HTTPS, and records acknowledgements.
//! * [`hub`]: pure functions the hub daemon calls from its `sync.offer` and `sync.chunk`
//!   handlers: offer verification, the want computation and a [`hub::ChunkAssembler`] that
//!   verifies each assembled object's BLAKE3 digest before handing it over.
//!
//! The protocol messages themselves (`Offer`, `Want`, `Item`) are `ogun_edge::sync`.

#![forbid(unsafe_code)]

pub mod edge;
pub mod hub;
pub mod keys;
pub mod signing;

/// Raw bytes per chunk on the HTTPS transport (O-11 section 6.3). Every chunk of an object except
/// the last is exactly this long. Base64 makes a chunk 699 052 bytes, which keeps one `sync.chunk`
/// request under the daemon's 1 MiB request line (O-15 section 3).
pub const CHUNK_BYTES: u64 = 512 * 1024;

/// The most items one offer may carry (O-11 section 6.2). An edge with more pending items sends
/// several offers.
pub const MAX_OFFER_ITEMS: usize = 1_000;

/// Number of chunks of an object of `size` bytes: `ceil(size / CHUNK_BYTES)`, and 1 for an empty
/// object (which travels as one empty chunk).
#[must_use]
pub fn chunk_count(size: u64) -> u64 {
    size.div_ceil(CHUNK_BYTES).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_counts() {
        assert_eq!(chunk_count(0), 1);
        assert_eq!(chunk_count(1), 1);
        assert_eq!(chunk_count(CHUNK_BYTES), 1);
        assert_eq!(chunk_count(CHUNK_BYTES + 1), 2);
        assert_eq!(chunk_count(3 * CHUNK_BYTES), 3);
    }
}
