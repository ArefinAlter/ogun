//! Edge-to-hub sync (O-11): a have/want protocol over content digests.
//!
//! Each edge is the only writer of the partitions under its edge id, so there are no merge
//! conflicts, only revisions. The edge offers what it has sealed; the hub answers with the digests
//! it lacks; the edge uploads those in resumable chunks; the hub verifies each blob's BLAKE3 before
//! committing it and only then acknowledges; only acknowledged items may be evicted at the edge.
//! Everything here is message types and pure state, so any transport (HTTPS with mTLS, or an S3
//! relay) can carry it.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Largest upload chunk.
pub const MAX_CHUNK_BYTES: u64 = 8 * 1024 * 1024;

/// One sealed window revision the edge holds.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Item {
    /// Asset name, for example the workflow and node that produced it.
    pub asset: String,
    /// Partition key, for example `w20261023T081500Z`.
    pub partition: String,
    /// Window revision, from 1.
    pub revision: u32,
    /// BLAKE3 of the blob, 64 lowercase hex.
    pub blake3: String,
    /// Blob size.
    pub size_bytes: u64,
}

/// `sync.offer`: what the edge has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    /// The edge identity (`edg_` id, O-00).
    pub edge_id: String,
    /// Items not yet acknowledged.
    pub items: Vec<Item>,
}

/// `sync.want`: the digests the hub lacks, with the bytes it already received of each, so an
/// interrupted upload resumes instead of restarting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Want {
    /// Digest to resume offset.
    pub blobs: BTreeMap<String, u64>,
    /// Digests the hub already holds and has acknowledged; the edge may evict them now.
    pub acked: BTreeSet<String>,
}

/// The hub's answer to an offer, given which blobs it holds completely and how far partial
/// uploads got.
#[must_use]
pub fn want(offer: &Offer, held: &BTreeSet<String>, partial: &BTreeMap<String, u64>) -> Want {
    let mut out = Want::default();
    for item in &offer.items {
        if held.contains(&item.blake3) {
            out.acked.insert(item.blake3.clone());
        } else {
            let offset = partial
                .get(&item.blake3)
                .copied()
                .unwrap_or(0)
                .min(item.size_bytes);
            out.blobs.insert(item.blake3.clone(), offset);
        }
    }
    out
}

/// One upload request: bytes `[offset, offset + len)` of a blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    /// Start offset.
    pub offset: u64,
    /// Length, at most [`MAX_CHUNK_BYTES`].
    pub len: u64,
}

/// The chunks still to send for a blob of `size` bytes when the hub already has `from` bytes.
#[must_use]
pub fn chunks(size: u64, from: u64, chunk_bytes: u64) -> Vec<Chunk> {
    let step = chunk_bytes.clamp(1, MAX_CHUNK_BYTES);
    let mut out = Vec::new();
    let mut offset = from.min(size);
    while offset < size {
        let len = step.min(size - offset);
        out.push(Chunk { offset, len });
        offset += len;
    }
    out
}

/// Why the hub refused a blob.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// The bytes do not hash to the offered digest.
    #[error("blob does not match its BLAKE3 digest")]
    DigestMismatch,
    /// The size differs from the offer.
    #[error("blob size differs from the offer")]
    SizeMismatch,
}

/// The hub's check before committing an uploaded blob.
///
/// # Errors
///
/// [`VerifyError`] when size or digest disagree with the offer; the hub then discards the partial
/// upload and wants the blob again from offset 0.
pub fn verify(item: &Item, bytes: &[u8]) -> Result<(), VerifyError> {
    if bytes.len() as u64 != item.size_bytes {
        return Err(VerifyError::SizeMismatch);
    }
    if blake3::hash(bytes).to_hex().as_str() != item.blake3 {
        return Err(VerifyError::DigestMismatch);
    }
    Ok(())
}

/// The edge's outbox: sealed items waiting for the hub's acknowledgement.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Outbox {
    pending: BTreeMap<String, Item>,
    acked: BTreeSet<String>,
}

impl Outbox {
    /// Queues a sealed item. Queuing the same digest twice is a no-op.
    pub fn push(&mut self, item: Item) {
        if !self.acked.contains(&item.blake3) {
            self.pending.entry(item.blake3.clone()).or_insert(item);
        }
    }

    /// The offer to send.
    #[must_use]
    pub fn offer(&self, edge_id: &str) -> Offer {
        Offer {
            edge_id: edge_id.to_owned(),
            items: self.pending.values().cloned().collect(),
        }
    }

    /// Applies the hub's acknowledgements. Returns the items that are now safe to evict.
    pub fn ack(&mut self, digests: &BTreeSet<String>) -> Vec<Item> {
        let mut out = Vec::new();
        for d in digests {
            if let Some(item) = self.pending.remove(d) {
                self.acked.insert(d.clone());
                out.push(item);
            }
        }
        out
    }

    /// Items waiting.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(bytes: &[u8], partition: &str) -> Item {
        Item {
            asset: "site-a/sensors".into(),
            partition: partition.into(),
            revision: 1,
            blake3: blake3::hash(bytes).to_hex().to_string(),
            size_bytes: bytes.len() as u64,
        }
    }

    #[test]
    fn hub_wants_only_what_it_lacks_and_resumes_partials() {
        let a = item(b"window a", "w1");
        let b = item(b"window b", "w2");
        let c = item(b"window c", "w3");
        let offer = Offer {
            edge_id: "edg_1".into(),
            items: vec![a.clone(), b.clone(), c.clone()],
        };
        let held = BTreeSet::from([a.blake3.clone()]);
        let partial = BTreeMap::from([(b.blake3.clone(), 3)]);
        let w = want(&offer, &held, &partial);
        assert_eq!(w.acked, BTreeSet::from([a.blake3.clone()]));
        assert_eq!(w.blobs.get(&b.blake3), Some(&3));
        assert_eq!(w.blobs.get(&c.blake3), Some(&0));
    }

    #[test]
    fn chunking_resumes_and_covers_exactly() {
        assert_eq!(
            chunks(10, 0, 4),
            vec![
                Chunk { offset: 0, len: 4 },
                Chunk { offset: 4, len: 4 },
                Chunk { offset: 8, len: 2 }
            ]
        );
        assert_eq!(chunks(10, 8, 4), vec![Chunk { offset: 8, len: 2 }]);
        assert!(chunks(10, 10, 4).is_empty());
        assert!(chunks(10, 99, 4).is_empty());
        let capped = chunks(3 * MAX_CHUNK_BYTES, 0, u64::MAX);
        assert_eq!(capped.len(), 3);
        assert!(capped.iter().all(|c| c.len == MAX_CHUNK_BYTES));
    }

    #[test]
    fn hub_refuses_tampered_or_truncated_blobs() {
        let a = item(b"window a", "w1");
        assert!(verify(&a, b"window a").is_ok());
        assert_eq!(verify(&a, b"window b"), Err(VerifyError::DigestMismatch));
        assert_eq!(verify(&a, b"window"), Err(VerifyError::SizeMismatch));
    }

    #[test]
    fn outbox_evicts_only_after_ack_and_ignores_duplicates() {
        let mut o = Outbox::default();
        let a = item(b"a", "w1");
        o.push(a.clone());
        o.push(a.clone());
        assert_eq!(o.pending(), 1);
        assert!(o.ack(&BTreeSet::from(["unknown".to_owned()])).is_empty());
        assert_eq!(o.ack(&BTreeSet::from([a.blake3.clone()])), vec![a.clone()]);
        o.push(a);
        assert_eq!(
            o.pending(),
            0,
            "an acknowledged digest is never offered again"
        );
    }
}
