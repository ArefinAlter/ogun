//! The hub side of sync (O-11 sections 3, 4 and 6): pure functions and a chunk store that the hub
//! daemon calls from its `sync.offer` and `sync.chunk` handlers. The sync bridge (`ogun-server`)
//! only relays and rate-limits; every check that matters happens here, in the daemon.
//!
//! A `sync.offer` handler does, in order:
//!
//! 1. look up the enrolled public key of `params.edge_id` (`edges.enroll`);
//! 2. [`verify_offer_params`]: signature, namespace and shape checks;
//! 3. [`answer_offer`]: records what is expected in the [`ChunkAssembler`] and computes the
//!    `Want` from the digests the hub's artifact store already holds. For a digest it holds, the
//!    daemon commits the item's lineage record (edge, asset, partition, revision, digest) before it
//!    acknowledges.
//!
//! A `sync.chunk` handler calls [`ChunkAssembler::put`]. On [`ChunkOutcome::Complete`] the daemon
//! imports the file into its artifact store, commits the item's lineage, and calls
//! [`ChunkAssembler::discard_complete`]; the next offer then lists the digest in `acked`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ogun_api::methods::{SyncChunkParams, SyncOfferParams};
use ogun_edge::sync::{Item, Offer, Want, want};
use ogun_types::{EdgeId, OgunError};
use serde_json::Value;

use crate::keys::parse_public_key;
use crate::signing::{SignatureError, verify_signature};
use crate::{CHUNK_BYTES, MAX_OFFER_ITEMS, chunk_count};

/// Why the hub refused an offer or a chunk. [`HubError::code`] is the O-00 code the daemon returns.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HubError {
    /// No enrolled key for this edge, or the enrolled key is unusable. `edge.not_enrolled`.
    #[error("this edge is not enrolled on the hub")]
    NotEnrolled,
    /// The offer signature does not verify under the enrolled key. `sync.bad_signature`.
    #[error("the offer signature does not verify under the enrolled key")]
    BadSignature,
    /// An item's asset is outside the offering edge's namespace (`<edge_id>/...`), or the signed
    /// offer names another edge. `api.permission_denied`.
    #[error("an offered item is outside this edge's namespace")]
    OutsideNamespace,
    /// The offer is malformed. `api.invalid_params`.
    #[error("malformed offer: {0}")]
    BadOffer(&'static str),
    /// The assembled object does not hash to the offered digest; the partial upload was discarded.
    /// `sync.digest_mismatch`.
    #[error("the uploaded object does not match its BLAKE3 digest; it was discarded")]
    DigestMismatch,
    /// The assembled object's size differs from the offer; the partial upload was discarded.
    /// `sync.digest_mismatch`.
    #[error("the uploaded object's size differs from the offer; it was discarded")]
    SizeMismatch,
    /// A chunk for a digest that no verified offer from this edge announced. `api.invalid_params`.
    #[error("no verified offer from this edge announced this digest")]
    NotOffered,
    /// A chunk is malformed (index, total, length or encoding). `api.invalid_params`.
    #[error("malformed chunk: {0}")]
    BadChunk(&'static str),
    /// The chunk store failed. `api.unavailable` (retryable).
    #[error("the chunk store failed: {0:?}")]
    Storage(std::io::ErrorKind),
}

impl HubError {
    /// The registered O-00 error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotEnrolled => "edge.not_enrolled",
            Self::BadSignature => "sync.bad_signature",
            Self::OutsideNamespace => "api.permission_denied",
            Self::DigestMismatch | Self::SizeMismatch => "sync.digest_mismatch",
            Self::BadOffer(_) | Self::NotOffered | Self::BadChunk(_) => "api.invalid_params",
            Self::Storage(_) => "api.unavailable",
        }
    }
}

impl From<HubError> for OgunError {
    fn from(e: HubError) -> Self {
        Self::new(e.code(), e.to_string())
    }
}

fn io(e: &std::io::Error) -> HubError {
    HubError::Storage(e.kind())
}

/// True for 64 lowercase hexadecimal characters.
#[must_use]
pub fn is_digest(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// True for a partition key of the O-02 static-key pattern `^[A-Za-z0-9._-]{1,64}$`, which window
/// keys (`wYYYYMMDDTHHMMSSZ`) match.
#[must_use]
pub fn is_partition_key(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// True when `asset` lies in the namespace of `edge_id`: `<edge_id>/<rest>` with a non-empty rest
/// of printable ASCII without `..` segments.
#[must_use]
pub fn in_namespace(edge_id: &str, asset: &str) -> bool {
    let Some(rest) = asset
        .strip_prefix(edge_id)
        .and_then(|r| r.strip_prefix('/'))
    else {
        return false;
    };
    !rest.is_empty()
        && rest.len() <= 256
        && rest.bytes().all(|b| b.is_ascii_graphic())
        && rest.split('/').all(|seg| !seg.is_empty() && seg != "..")
}

/// Verifies a signed offer (O-11 section 6.1) and checks its shape and namespace.
///
/// # Errors
///
/// [`HubError::NotEnrolled`] for an unusable key, [`HubError::BadSignature`],
/// [`HubError::BadOffer`] or [`HubError::OutsideNamespace`].
pub fn verify_offer(
    public_key_b64: &str,
    offer_json: &Value,
    signature_b64: &str,
) -> Result<Offer, HubError> {
    let key = parse_public_key(public_key_b64).ok_or(HubError::NotEnrolled)?;
    verify_signature(&key, offer_json, signature_b64).map_err(|e| match e {
        SignatureError::NotCanonical => HubError::BadOffer("not canonical JSON"),
        SignatureError::Invalid => HubError::BadSignature,
    })?;
    let offer: Offer = serde_json::from_value(offer_json.clone())
        .map_err(|_| HubError::BadOffer("not an offer"))?;
    if offer.edge_id.parse::<EdgeId>().is_err() {
        return Err(HubError::BadOffer("edge_id is not an edg_ id"));
    }
    if offer.items.len() > MAX_OFFER_ITEMS {
        return Err(HubError::BadOffer("too many items"));
    }
    for item in &offer.items {
        if !in_namespace(&offer.edge_id, &item.asset) {
            return Err(HubError::OutsideNamespace);
        }
        if !is_digest(&item.blake3) {
            return Err(HubError::BadOffer("blake3 is not 64 lowercase hex"));
        }
        if !is_partition_key(&item.partition) {
            return Err(HubError::BadOffer("partition is not a partition key"));
        }
        if item.revision == 0 {
            return Err(HubError::BadOffer("revisions start at 1"));
        }
    }
    Ok(offer)
}

/// [`verify_offer`] for the daemon's `sync.offer` parameters, with the key enrolled for
/// `params.edge_id` (`None` when there is none). Also checks that the signed offer names the same
/// edge as the request.
///
/// # Errors
///
/// As [`verify_offer`]; [`HubError::NotEnrolled`] when `enrolled_public_key` is `None`.
pub fn verify_offer_params(
    params: &SyncOfferParams,
    enrolled_public_key: Option<&str>,
) -> Result<Offer, HubError> {
    let key = enrolled_public_key.ok_or(HubError::NotEnrolled)?;
    let offer = verify_offer(key, &params.offer, &params.signature)?;
    if offer.edge_id != params.edge_id.to_string() {
        return Err(HubError::OutsideNamespace);
    }
    Ok(offer)
}

/// The hub's answer to a verified offer. `held` is the set of digests the hub's artifact store
/// holds completely (and whose lineage for this edge it has committed). Every other item is
/// recorded in `assembler` as expected, and its resume offset comes from the chunks already there.
///
/// # Errors
///
/// [`HubError::Storage`] when the chunk store cannot record an expectation.
pub fn answer_offer(
    assembler: &ChunkAssembler,
    offer: &Offer,
    held: &BTreeSet<String>,
) -> Result<Want, HubError> {
    let mut partial = BTreeMap::new();
    for item in offer.items.iter().filter(|i| !held.contains(&i.blake3)) {
        assembler.expect(&offer.edge_id, item)?;
        partial.insert(
            item.blake3.clone(),
            assembler.resume_offset(&offer.edge_id, item),
        );
    }
    Ok(want(offer, held, &partial))
}

/// What a chunk did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkOutcome {
    /// More chunks are needed.
    Partial {
        /// Chunks stored so far.
        received: u32,
        /// Chunks in total.
        total: u32,
    },
    /// The object is complete and its size and BLAKE3 digest match the offer.
    Complete {
        /// The assembled object. The daemon imports it into its artifact store.
        path: PathBuf,
        /// The offered item it belongs to.
        item: Item,
    },
}

/// Stores uploaded chunks and assembles verified objects (O-11 section 6.3).
///
/// Layout under the root directory (all `0700` on Unix):
///
/// ```text
/// <root>/<edge_id>/<blake3>/item.json        the offered item, written when an offer wants it
/// <root>/<edge_id>/<blake3>/<index>.chunk    one file per received chunk (index zero-padded)
/// <root>/<edge_id>/complete/<blake3>         the assembled, verified object
/// <root>/<edge_id>/complete/<blake3>.json    its item
/// ```
///
/// Every write goes to a temporary file first and is renamed into place, so a crash leaves either
/// a whole chunk or none. Repeating a chunk is harmless; a chunk for an object that is already
/// complete returns the complete object again.
#[derive(Debug, Clone)]
pub struct ChunkAssembler {
    root: PathBuf,
}

fn create_dir(dir: &Path) -> Result<(), HubError> {
    crate::keys::create_private_dir(dir).map_err(|e| match e {
        crate::keys::KeyError::Io(kind) => HubError::Storage(kind),
        _ => HubError::Storage(std::io::ErrorKind::Other),
    })
}

fn temp_name(base: &Path) -> Result<PathBuf, HubError> {
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| HubError::Storage(std::io::ErrorKind::Other))?;
    let suffix: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let mut name = base.as_os_str().to_owned();
    name.push(format!(".tmp-{suffix}"));
    Ok(PathBuf::from(name))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), HubError> {
    let tmp = temp_name(path)?;
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(io(&e));
    }
    Ok(())
}

fn read_item(path: &Path) -> Result<Option<Item>, HubError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io(&e)),
    }
}

fn remove_dir(dir: &Path) -> Result<(), HubError> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io(&e)),
    }
}

fn check_edge(edge_id: &str) -> Result<(), HubError> {
    edge_id
        .parse::<EdgeId>()
        .map(|_| ())
        .map_err(|_| HubError::BadOffer("edge_id is not an edg_ id"))
}

impl ChunkAssembler {
    /// Opens (and creates) the chunk store at `root`.
    ///
    /// # Errors
    ///
    /// [`HubError::Storage`] when the directory cannot be created.
    pub fn open(root: &Path) -> Result<Self, HubError> {
        create_dir(root)?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    fn object_dir(&self, edge_id: &str, digest: &str) -> PathBuf {
        self.root.join(edge_id).join(digest)
    }

    fn complete_path(&self, edge_id: &str, digest: &str) -> PathBuf {
        self.root.join(edge_id).join("complete").join(digest)
    }

    fn chunk_path(dir: &Path, index: u64) -> PathBuf {
        dir.join(format!("{index:08}.chunk"))
    }

    /// Records that `item` is wanted from `edge_id`. A changed size or partition for the same
    /// digest replaces the record and discards chunks received under the old one.
    ///
    /// # Errors
    ///
    /// [`HubError::BadOffer`] for an invalid edge id or digest, [`HubError::Storage`].
    pub fn expect(&self, edge_id: &str, item: &Item) -> Result<(), HubError> {
        check_edge(edge_id)?;
        if !is_digest(&item.blake3) {
            return Err(HubError::BadOffer("blake3 is not 64 lowercase hex"));
        }
        let dir = self.object_dir(edge_id, &item.blake3);
        let item_file = dir.join("item.json");
        if let Some(known) = read_item(&item_file)? {
            if known == *item {
                return Ok(());
            }
            if known.size_bytes != item.size_bytes {
                remove_dir(&dir)?;
            }
        }
        create_dir(&dir)?;
        let bytes = serde_json::to_vec(item).map_err(|_| HubError::BadOffer("not an offer"))?;
        write_atomic(&item_file, &bytes)
    }

    /// Bytes of `item` the hub holds contiguously from offset 0: whole chunks received in order,
    /// or the full size when the object is already assembled.
    #[must_use]
    pub fn resume_offset(&self, edge_id: &str, item: &Item) -> u64 {
        if self.complete_path(edge_id, &item.blake3).is_file() {
            return item.size_bytes;
        }
        let dir = self.object_dir(edge_id, &item.blake3);
        let total = chunk_count(item.size_bytes);
        let mut contiguous = 0;
        while contiguous < total && Self::chunk_path(&dir, contiguous).is_file() {
            contiguous += 1;
        }
        (contiguous * CHUNK_BYTES).min(item.size_bytes)
    }

    /// Stores one chunk and, when it was the last one missing, assembles and verifies the object.
    ///
    /// # Errors
    ///
    /// [`HubError::NotOffered`], [`HubError::BadChunk`], [`HubError::DigestMismatch`] or
    /// [`HubError::SizeMismatch`] (the partial upload is discarded then), [`HubError::Storage`].
    pub fn put(&self, chunk: &SyncChunkParams) -> Result<ChunkOutcome, HubError> {
        let edge_id = chunk.edge_id.to_string();
        if !is_digest(&chunk.blake3) {
            return Err(HubError::BadChunk("blake3 is not 64 lowercase hex"));
        }
        let complete = self.complete_path(&edge_id, &chunk.blake3);
        let mut complete_item = complete.clone();
        complete_item.set_extension("json");
        if complete.is_file()
            && let Some(item) = read_item(&complete_item)?
        {
            return Ok(ChunkOutcome::Complete {
                path: complete,
                item,
            });
        }
        let dir = self.object_dir(&edge_id, &chunk.blake3);
        let item = read_item(&dir.join("item.json"))?.ok_or(HubError::NotOffered)?;
        if item.partition != chunk.partition {
            return Err(HubError::BadChunk("partition differs from the offer"));
        }
        let total = chunk_count(item.size_bytes);
        if u64::from(chunk.total) != total {
            return Err(HubError::BadChunk("total differs from the offered size"));
        }
        let index = u64::from(chunk.index);
        if index >= total {
            return Err(HubError::BadChunk("index is not below total"));
        }
        let max_encoded = CHUNK_BYTES.div_ceil(3) * 4;
        if chunk.data_base64.len() as u64 > max_encoded {
            return Err(HubError::BadChunk("chunk is larger than 512 KiB"));
        }
        let data = STANDARD
            .decode(chunk.data_base64.as_bytes())
            .map_err(|_| HubError::BadChunk("data_base64 is not base64"))?;
        let expected_len = if index + 1 == total {
            item.size_bytes - index * CHUNK_BYTES
        } else {
            CHUNK_BYTES
        };
        if data.len() as u64 != expected_len {
            return Err(HubError::BadChunk("chunk length is wrong for its index"));
        }
        write_atomic(&Self::chunk_path(&dir, index), &data)?;
        let received = (0..total)
            .filter(|i| Self::chunk_path(&dir, *i).is_file())
            .count() as u64;
        if received < total {
            return Ok(ChunkOutcome::Partial {
                received: u32::try_from(received).unwrap_or(u32::MAX),
                total: chunk.total,
            });
        }
        self.assemble(&edge_id, &dir, &item, total)
    }

    fn assemble(
        &self,
        edge_id: &str,
        dir: &Path,
        item: &Item,
        total: u64,
    ) -> Result<ChunkOutcome, HubError> {
        let complete = self.complete_path(edge_id, &item.blake3);
        if let Some(parent) = complete.parent() {
            create_dir(parent)?;
        }
        let tmp = temp_name(&complete)?;
        let assembled = (|| -> Result<(u64, blake3::Hash), std::io::Error> {
            let mut out = std::fs::File::create(&tmp)?;
            let mut hasher = blake3::Hasher::new();
            let mut size = 0u64;
            let mut buf = Vec::with_capacity(usize::try_from(CHUNK_BYTES).unwrap_or(0));
            for i in 0..total {
                buf.clear();
                std::fs::File::open(Self::chunk_path(dir, i))?.read_to_end(&mut buf)?;
                hasher.update(&buf);
                out.write_all(&buf)?;
                size += buf.len() as u64;
            }
            out.sync_all()?;
            Ok((size, hasher.finalize()))
        })();
        let (size, digest) = match assembled {
            Ok(v) => v,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(io(&e));
            }
        };
        let mismatch = if size != item.size_bytes {
            Some(HubError::SizeMismatch)
        } else if digest.to_hex().as_str() != item.blake3 {
            Some(HubError::DigestMismatch)
        } else {
            None
        };
        if let Some(err) = mismatch {
            let _ = std::fs::remove_file(&tmp);
            // Discard the chunks but keep the expectation, so the next offer wants it from 0.
            for i in 0..total {
                let _ = std::fs::remove_file(Self::chunk_path(dir, i));
            }
            return Err(err);
        }
        let mut item_path = complete.clone();
        item_path.set_extension("json");
        let item_bytes =
            serde_json::to_vec(item).map_err(|_| HubError::BadOffer("not an offer"))?;
        write_atomic(&item_path, &item_bytes)?;
        std::fs::rename(&tmp, &complete).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            io(&e)
        })?;
        remove_dir(dir)?;
        Ok(ChunkOutcome::Complete {
            path: complete,
            item: item.clone(),
        })
    }

    /// Removes an assembled object after the daemon imported it into its artifact store.
    ///
    /// # Errors
    ///
    /// [`HubError::Storage`].
    pub fn discard_complete(&self, edge_id: &str, digest: &str) -> Result<(), HubError> {
        check_edge(edge_id)?;
        if !is_digest(digest) {
            return Err(HubError::BadChunk("blake3 is not 64 lowercase hex"));
        }
        let complete = self.complete_path(edge_id, digest);
        let mut item_path = complete.clone();
        item_path.set_extension("json");
        for p in [complete, item_path] {
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io(&e)),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::EdgeKey;
    use crate::signing::sign_offer;
    use ogun_types::EdgeId;

    const EDGE: &str = "edg_01M45MN5M0PZ0D9W0APDCGRNZD";

    fn item(bytes: &[u8], partition: &str) -> Item {
        Item {
            asset: format!("{EDGE}/air-quality-stream/sensors.window"),
            partition: partition.into(),
            revision: 1,
            blake3: blake3::hash(bytes).to_hex().to_string(),
            size_bytes: bytes.len() as u64,
        }
    }

    fn params(offer: &Offer, key: &EdgeKey) -> SyncOfferParams {
        let (value, signature) = sign_offer(key, offer).unwrap();
        SyncOfferParams {
            edge_id: EDGE.parse::<EdgeId>().unwrap(),
            offer: value,
            signature,
        }
    }

    fn chunk(item: &Item, bytes: &[u8], index: u64) -> SyncChunkParams {
        let start = usize::try_from(index * CHUNK_BYTES).unwrap();
        let end = (start + usize::try_from(CHUNK_BYTES).unwrap()).min(bytes.len());
        SyncChunkParams {
            edge_id: EDGE.parse::<EdgeId>().unwrap(),
            partition: item.partition.clone(),
            blake3: item.blake3.clone(),
            index: u32::try_from(index).unwrap(),
            total: u32::try_from(chunk_count(item.size_bytes)).unwrap(),
            data_base64: STANDARD.encode(&bytes[start..end]),
        }
    }

    #[test]
    fn offers_verify_only_for_the_enrolled_key_and_namespace() {
        let key = EdgeKey::from_seed(&[1; 32]);
        let offer = Offer {
            edge_id: EDGE.into(),
            items: vec![item(b"a", "w20261023T081500Z")],
        };
        let p = params(&offer, &key);
        assert_eq!(
            verify_offer_params(&p, Some(&key.public_key())).unwrap(),
            offer
        );
        assert_eq!(verify_offer_params(&p, None), Err(HubError::NotEnrolled));
        let other = EdgeKey::from_seed(&[2; 32]);
        assert_eq!(
            verify_offer_params(&p, Some(&other.public_key())),
            Err(HubError::BadSignature)
        );
        let mut foreign = offer.clone();
        foreign.items[0].asset = "edg_01M45MN5M0PZ0D9W0APDCGRNZE/x/sensors.window".into();
        assert_eq!(
            verify_offer_params(&params(&foreign, &key), Some(&key.public_key())),
            Err(HubError::OutsideNamespace)
        );
        assert_eq!(HubError::BadSignature.code(), "sync.bad_signature");
        assert_eq!(HubError::NotEnrolled.code(), "edge.not_enrolled");
        assert_eq!(HubError::DigestMismatch.code(), "sync.digest_mismatch");
    }

    #[test]
    fn chunks_assemble_resume_and_verify() {
        let dir = tempfile::tempdir().unwrap();
        let hub = ChunkAssembler::open(dir.path()).unwrap();
        let bytes: Vec<u8> = (0..(2 * CHUNK_BYTES + 10))
            .map(|i| (i % 251) as u8)
            .collect();
        let it = item(&bytes, "w20261023T081500Z");
        let offer = Offer {
            edge_id: EDGE.into(),
            items: vec![it.clone()],
        };
        let w = answer_offer(&hub, &offer, &BTreeSet::new()).unwrap();
        assert_eq!(w.blobs.get(&it.blake3), Some(&0));
        assert!(matches!(
            hub.put(&chunk(&it, &bytes, 0)).unwrap(),
            ChunkOutcome::Partial {
                received: 1,
                total: 3
            }
        ));
        // The link drops; the next offer resumes after the first chunk.
        let w = answer_offer(&hub, &offer, &BTreeSet::new()).unwrap();
        assert_eq!(w.blobs.get(&it.blake3), Some(&CHUNK_BYTES));
        hub.put(&chunk(&it, &bytes, 1)).unwrap();
        let ChunkOutcome::Complete { path, item: done } = hub.put(&chunk(&it, &bytes, 2)).unwrap()
        else {
            panic!("expected a complete object");
        };
        assert_eq!(done, it);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        // A repeated last chunk is answered with the same complete object.
        assert!(matches!(
            hub.put(&chunk(&it, &bytes, 2)).unwrap(),
            ChunkOutcome::Complete { .. }
        ));
        hub.discard_complete(EDGE, &it.blake3).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn tampered_objects_are_discarded_and_wanted_again() {
        let dir = tempfile::tempdir().unwrap();
        let hub = ChunkAssembler::open(dir.path()).unwrap();
        let bytes = b"window bytes".to_vec();
        let it = item(&bytes, "w20261023T081500Z");
        let offer = Offer {
            edge_id: EDGE.into(),
            items: vec![it.clone()],
        };
        answer_offer(&hub, &offer, &BTreeSet::new()).unwrap();
        let mut bad = chunk(&it, b"window BYTES", 0);
        bad.blake3 = it.blake3.clone();
        assert_eq!(hub.put(&bad), Err(HubError::DigestMismatch));
        let w = answer_offer(&hub, &offer, &BTreeSet::new()).unwrap();
        assert_eq!(w.blobs.get(&it.blake3), Some(&0));
        assert!(matches!(
            hub.put(&chunk(&it, &bytes, 0)).unwrap(),
            ChunkOutcome::Complete { .. }
        ));
    }

    #[test]
    fn chunks_without_an_offer_or_with_wrong_shape_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let hub = ChunkAssembler::open(dir.path()).unwrap();
        let bytes = b"abc".to_vec();
        let it = item(&bytes, "w20261023T081500Z");
        assert_eq!(hub.put(&chunk(&it, &bytes, 0)), Err(HubError::NotOffered));
        hub.expect(EDGE, &it).unwrap();
        let mut wrong_total = chunk(&it, &bytes, 0);
        wrong_total.total = 2;
        assert!(matches!(hub.put(&wrong_total), Err(HubError::BadChunk(_))));
        let mut wrong_partition = chunk(&it, &bytes, 0);
        wrong_partition.partition = "w20261023T081600Z".into();
        assert!(matches!(
            hub.put(&wrong_partition),
            Err(HubError::BadChunk(_))
        ));
    }

    #[test]
    fn held_digests_are_acknowledged_without_upload() {
        let dir = tempfile::tempdir().unwrap();
        let hub = ChunkAssembler::open(dir.path()).unwrap();
        let it = item(b"x", "w20261023T081500Z");
        let offer = Offer {
            edge_id: EDGE.into(),
            items: vec![it.clone()],
        };
        let w = answer_offer(&hub, &offer, &BTreeSet::from([it.blake3.clone()])).unwrap();
        assert!(w.blobs.is_empty());
        assert!(w.acked.contains(&it.blake3));
    }
}
