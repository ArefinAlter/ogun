//! Exercise provenance through the daemon's real sync handler and artifact store.
use base64::Engine;
use ogun_api::{OgunPaths, methods as m};
use ogun_edge::{Item, Offer};
use ogun_types::{EdgeId, IdGenerator, Timestamp};
use serde_json::{Value, json};

fn enroll(d: &ogun_daemon::Daemon, id: EdgeId, key: &ogun_sync::keys::EdgeKey) {
    d.store
        .put_doc(
            ogun_store::Collection::Edges,
            &id.to_string(),
            &m::EdgeView {
                edge_id: id,
                name: "test".into(),
                public_key: key.public_key(),
                enrolled_at: Timestamp::now(),
                last_sync_at: None,
            },
        )
        .unwrap();
}

fn offer(
    d: &ogun_daemon::Daemon,
    id: EdgeId,
    key: &ogun_sync::keys::EdgeKey,
    items: Vec<Item>,
) -> ogun_api::ApiResult<Value> {
    let (offer, signature) = ogun_sync::signing::sign_offer(
        key,
        &Offer {
            edge_id: id.to_string(),
            items,
        },
    )
    .unwrap();
    ogun_daemon::sync::handle(
        d,
        m::name::SYNC_OFFER,
        json!({"edge_id": id, "offer": offer, "signature": signature}),
    )
}

fn item(id: EdgeId, bytes: &[u8], asset: &str, revision: u32) -> Item {
    Item {
        asset: format!("{id}/{asset}"),
        partition: "w20261023T081500Z".into(),
        revision,
        blake3: blake3::hash(bytes).to_hex().to_string(),
        size_bytes: bytes.len() as u64,
    }
}

#[test]
fn shared_bytes_preserve_all_offered_identities_before_acknowledgement() {
    let dir = tempfile::tempdir().unwrap();
    let d = ogun_daemon::Daemon::open(OgunPaths::rooted(dir.path(), None)).unwrap();
    let ids = IdGenerator::new();
    let edge = ids.edge_id();
    let other_edge = ids.edge_id();
    let key = ogun_sync::keys::EdgeKey::from_seed(&[17; 32]);
    enroll(&d, edge, &key);
    enroll(&d, other_edge, &key);
    let bytes = b"{\"value\":1}\n";
    let first = item(edge, bytes, "flow/sensors", 1);
    let second = item(edge, bytes, "flow/other", 1);
    let initial = vec![first.clone(), second.clone()];
    let want = offer(&d, edge, &key, initial.clone()).unwrap();
    assert_eq!(want["want"]["blobs"][&first.blake3], 0);
    let done = ogun_daemon::sync::handle(
        &d,
        m::name::SYNC_CHUNK,
        json!({
            "edge_id": edge, "partition": second.partition, "blake3": first.blake3,
            "index": 0, "total": 1,
            "data_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
        }),
    )
    .unwrap();
    assert_eq!(done["complete"], true);
    let mut identities = initial;
    identities.push(item(edge, bytes, "flow/sensors", 2));
    let mut another_partition = first.clone();
    another_partition.partition = "w20261023T081600Z".into();
    identities.push(another_partition);
    for _ in 0..2 {
        let ack = offer(&d, edge, &key, identities.clone()).unwrap();
        assert_eq!(ack["want"]["acked"], json!([first.blake3]));
        assert_eq!(ack["want"]["blobs"], json!({}));
    }
    let foreign = item(other_edge, bytes, "flow/sensors", 1);
    for _ in 0..2 {
        let ack = offer(&d, other_edge, &key, vec![foreign.clone()]).unwrap();
        assert_eq!(ack["want"]["acked"], json!([first.blake3]));
    }
    let records = d
        .artifacts
        .list(&zeus_artifact::ArtifactFilter {
            limit: 100,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        records.len(),
        5,
        "repeated offers create no duplicate lineage records"
    );
    assert!(
        records
            .iter()
            .all(|a| a.retention == zeus_artifact::Retention::Keep)
    );
    identities.push(foreign);
    for identity in identities {
        assert_eq!(
            records
                .iter()
                .filter(|a| {
                    a.metadata["ogun.asset"] == identity.asset
                        && a.metadata["ogun.partition"] == identity.partition
                        && a.metadata["ogun.revision"] == identity.revision
                        && a.metadata["ogun.edge"] == identity.asset.split('/').next().unwrap()
                })
                .count(),
            1
        );
    }
    let mut wrong_size = first.clone();
    wrong_size.size_bytes += 1;
    assert_eq!(
        offer(&d, edge, &key, vec![first.clone(), wrong_size])
            .unwrap_err()
            .code(),
        "sync.digest_mismatch"
    );
    // A catalog row must not cause an acknowledgement once its object has disappeared.
    let object_path = d.artifacts.object_path(records[0].blake3);
    #[cfg(windows)]
    {
        // The CAS deliberately marks objects read-only; Windows refuses to unlink them until
        // that attribute is cleared. This simulates loss of the object, not a normal API delete.
        let mut permissions = std::fs::metadata(&object_path).unwrap().permissions();
        // Windows-only: the attribute is the thing under test, and there is no Unix mode to widen.
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&object_path, permissions).unwrap();
    }
    std::fs::remove_file(object_path).unwrap();
    let want = offer(&d, edge, &key, vec![first.clone()]).unwrap();
    assert_eq!(want["want"]["acked"], json!([]));
    assert_eq!(want["want"]["blobs"][&first.blake3], 0);
    let error = ogun_daemon::sync::handle(
        &d,
        m::name::SYNC_CHUNK,
        json!({
            "edge_id": edge, "partition": first.partition, "blake3": first.blake3,
            "index": 0, "total": 1,
            "data_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
        }),
    )
    .unwrap_err();
    assert_eq!(
        error.code(),
        "api.unavailable",
        "key reuse cannot falsely report an unavailable CAS object as complete"
    );
}
