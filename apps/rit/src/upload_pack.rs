//! From-scratch **upload-pack**: the fetch/clone side of the git wire protocol,
//! served entirely from `rit`'s object store — no `git upload-pack` bridge.
//!
//! Three pieces:
//! - [`advertise_upload_pack`] — the v0 ref advertisement a client reads first;
//! - [`reachable_objects`] — the object set for a `want`/`have` negotiation
//!   (everything reachable from the wants, minus everything reachable from the
//!   haves), computed by walking commits → trees → blobs (+ tags);
//! - [`generate_pack`] — a real, self-describing **packfile** (`PACK` header,
//!   per-object type/size varint + zlib payload, SHA-1 trailer). Objects are
//!   emitted **undeltified** (full), which is a valid pack `git index-pack`
//!   accepts and `git fsck` validates.
//!
//! [`upload_pack_v0`] ties them together for the stateless-RPC (HTTP) case:
//! parse the request, compute the set, and return `NAK` + the pack.

use std::collections::{HashSet, VecDeque};

use crate::error::{ForgeError, Result};
use crate::object::{Object, ObjectType};
use crate::oid::ObjectId;
use crate::repo::Repository;
use crate::wire::{decode_packet_lines, encode_packet_line, PacketLine, PacketLineLimits};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use sha1::{Digest, Sha1};
use std::io::Write;

/// Capabilities `rit` advertises for upload-pack. Intentionally minimal: no
/// multi-ack and no side-band, so the negotiation is a single `NAK` + raw pack.
pub const UPLOAD_PACK_CAPABILITIES: &[&str] = &["agent=rit/0.1", "object-format=sha1"];

/// Upper bound on objects walked/sent (fail-closed against pathological repos).
const MAX_OBJECTS: usize = 5_000_000;

/// Produce the **v0 upload-pack ref advertisement**: `HEAD` first (carrying the
/// capabilities and a `symref=HEAD:<branch>` hint), then every ref, then flush.
pub fn advertise_upload_pack(repo: &Repository) -> Result<Vec<u8>> {
    let mut refs: Vec<(String, ObjectId)> = Vec::new();
    if let Ok(head) = repo.refs().resolve_head() {
        refs.push(("HEAD".to_string(), head));
    }
    refs.extend(repo.refs().list()?);
    refs.sort_by(|a, b| a.0.cmp(&b.0));
    refs.dedup_by(|a, b| a.0 == b.0);

    // A `symref=HEAD:refs/heads/<branch>` cap lets `clone` check out the right
    // branch; derive it from the HEAD symbolic ref when present.
    let mut caps: Vec<String> = UPLOAD_PACK_CAPABILITIES.iter().map(|c| c.to_string()).collect();
    if let Ok(crate::RefTarget::Symbolic(target)) = repo.refs().read_head() {
        caps.push(format!("symref=HEAD:{target}"));
    }
    let capability_list = caps.join(" ");

    let mut out = Vec::new();
    if refs.is_empty() {
        // An empty repository advertises the zero-id capabilities pseudo-ref.
        let line = format!(
            "0000000000000000000000000000000000000000 capabilities^{{}}\0{capability_list}\n"
        );
        out.extend(encode_packet_line(&PacketLine::Data(line.into_bytes()))?);
    } else {
        for (index, (name, oid)) in refs.into_iter().enumerate() {
            let line = if index == 0 {
                format!("{oid} {name}\0{capability_list}\n")
            } else {
                format!("{oid} {name}\n")
            };
            out.extend(encode_packet_line(&PacketLine::Data(line.into_bytes()))?);
        }
    }
    out.extend_from_slice(b"0000");
    Ok(out)
}

/// The parsed contents of an upload-pack request body.
#[derive(Default, Debug)]
pub struct FetchRequest {
    pub wants: Vec<ObjectId>,
    pub haves: Vec<ObjectId>,
    pub done: bool,
}

/// Parse `want`/`have`/`done` pkt-lines out of an upload-pack request body.
pub fn parse_fetch_request(body: &[u8]) -> Result<FetchRequest> {
    let lines = decode_packet_lines(body, PacketLineLimits::default())?;
    let mut request = FetchRequest::default();
    for line in lines {
        if let PacketLine::Data(data) = line {
            let text = String::from_utf8_lossy(&data);
            let text = text.trim_end();
            if let Some(rest) = text.strip_prefix("want ") {
                if let Some(hex) = rest.split_whitespace().next() {
                    request.wants.push(ObjectId::from_hex(hex)?);
                }
            } else if let Some(rest) = text.strip_prefix("have ") {
                if let Some(hex) = rest.split_whitespace().next() {
                    if let Ok(oid) = ObjectId::from_hex(hex) {
                        request.haves.push(oid);
                    }
                }
            } else if text == "done" {
                request.done = true;
            }
        }
    }
    Ok(request)
}

/// Serve a **stateless** (HTTP smart) upload-pack request: parse it, compute the
/// object set, and return `NAK` followed by the raw packfile.
pub fn upload_pack_v0(repo: &Repository, body: &[u8]) -> Result<Vec<u8>> {
    let request = parse_fetch_request(body)?;
    if request.wants.is_empty() {
        // Nothing requested — a bare flush is a valid "no-op" response.
        return Ok(b"0000".to_vec());
    }
    let send = reachable_objects(repo, &request.wants, &request.haves)?;
    let pack = generate_pack(repo, &send)?;
    let mut out = encode_packet_line(&PacketLine::Data(b"NAK\n".to_vec()))?;
    out.extend(pack);
    Ok(out)
}

/// Objects reachable from `wants` but not from `haves` (the fetch send set).
pub fn reachable_objects(
    repo: &Repository,
    wants: &[ObjectId],
    haves: &[ObjectId],
) -> Result<Vec<ObjectId>> {
    // Everything the client already has — pruned from the walk.
    let mut seen: HashSet<ObjectId> = HashSet::new();
    if !haves.is_empty() {
        let mut sink = Vec::new();
        walk(repo, haves, &mut seen, &mut sink)?;
    }
    // Now collect the new objects reachable from the wants.
    let mut send = Vec::new();
    walk(repo, wants, &mut seen, &mut send)?;
    Ok(send)
}

/// Walk the object graph from `roots`, adding newly-seen oids to `out`. `seen`
/// is pre-seeded with objects to skip (the have-closure), so those are pruned.
fn walk(
    repo: &Repository,
    roots: &[ObjectId],
    seen: &mut HashSet<ObjectId>,
    out: &mut Vec<ObjectId>,
) -> Result<()> {
    let mut queue: VecDeque<ObjectId> = roots.iter().copied().collect();
    while let Some(oid) = queue.pop_front() {
        if !seen.insert(oid) {
            continue;
        }
        if seen.len() > MAX_OBJECTS {
            return Err(ForgeError::ResourceLimit("fetch object walk too large".into()));
        }
        out.push(oid);
        match repo.odb().read(&oid)? {
            Object::Commit(commit) => {
                walk_tree(repo, commit.tree, seen, out)?;
                for parent in commit.parents {
                    queue.push_back(parent);
                }
            }
            Object::Tag(tag) => queue.push_back(tag.object),
            Object::Tree(_) => walk_tree(repo, oid, seen, out)?,
            Object::Blob(_) => {}
        }
    }
    Ok(())
}

/// Add a tree and everything under it (subtrees + blobs) to `seen`/`out`.
fn walk_tree(
    repo: &Repository,
    tree_oid: ObjectId,
    seen: &mut HashSet<ObjectId>,
    out: &mut Vec<ObjectId>,
) -> Result<()> {
    let mut stack = vec![tree_oid];
    while let Some(oid) = stack.pop() {
        if !seen.insert(oid) {
            continue;
        }
        if seen.len() > MAX_OBJECTS {
            return Err(ForgeError::ResourceLimit("fetch tree walk too large".into()));
        }
        out.push(oid);
        for entry in repo.tree(&oid)?.entries {
            match entry.kind() {
                "tree" => stack.push(entry.oid),
                "commit" => { /* gitlink: not part of this repo's objects */ }
                _ => {
                    if seen.insert(entry.oid) {
                        out.push(entry.oid);
                    }
                }
            }
        }
    }
    Ok(())
}

/// The pack object type code for an object type.
fn type_code(object_type: ObjectType) -> u8 {
    match object_type {
        ObjectType::Commit => 1,
        ObjectType::Tree => 2,
        ObjectType::Blob => 3,
        ObjectType::Tag => 4,
    }
}

/// Encode a pack object header (type + size, git's base-128 varint with the
/// type in the first byte).
fn encode_object_header(type_code: u8, size: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut byte = ((type_code & 0x07) << 4) | (size & 0x0f) as u8;
    let mut remaining = size >> 4;
    while remaining > 0 {
        out.push(byte | 0x80);
        byte = (remaining & 0x7f) as u8;
        remaining >>= 7;
    }
    out.push(byte);
    out
}

/// Build a full (undeltified) packfile containing exactly `oids`.
pub fn generate_pack(repo: &Repository, oids: &[ObjectId]) -> Result<Vec<u8>> {
    let count = u32::try_from(oids.len())
        .map_err(|_| ForgeError::ResourceLimit("pack object count exceeds u32".into()))?;
    let mut pack: Vec<u8> = Vec::new();
    pack.extend_from_slice(b"PACK");
    pack.extend_from_slice(&2u32.to_be_bytes());
    pack.extend_from_slice(&count.to_be_bytes());

    for oid in oids {
        let (object_type, payload) = repo.odb().read_raw(oid)?;
        pack.extend(encode_object_header(type_code(object_type), payload.len()));
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&payload)?;
        let compressed = encoder
            .finish()
            .map_err(|e| ForgeError::Decompress(format!("pack compression failed: {e}")))?;
        pack.extend(compressed);
    }

    // Trailing SHA-1 over the whole pack (excluding the trailer itself).
    let mut hasher = Sha1::new();
    hasher.update(&pack);
    pack.extend_from_slice(&hasher.finalize());
    Ok(pack)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::ObjectType;

    #[test]
    fn pack_header_and_trailer_are_wellformed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();
        let a = repo.hash_object(ObjectType::Blob, b"alpha\n", true).unwrap();
        let b = repo.hash_object(ObjectType::Blob, b"beta\n", true).unwrap();
        let pack = generate_pack(&repo, &[a, b]).unwrap();

        assert_eq!(&pack[0..4], b"PACK");
        assert_eq!(u32::from_be_bytes(pack[4..8].try_into().unwrap()), 2);
        assert_eq!(u32::from_be_bytes(pack[8..12].try_into().unwrap()), 2);
        // Trailer is the SHA-1 of everything before it.
        let body = &pack[..pack.len() - 20];
        let mut h = Sha1::new();
        h.update(body);
        assert_eq!(&pack[pack.len() - 20..], h.finalize().as_slice());
    }

    #[test]
    fn reachable_set_covers_commit_tree_blob() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();
        let blob = repo.hash_object(ObjectType::Blob, b"x\n", true).unwrap();
        let mut tree = crate::Tree {
            entries: vec![crate::tree::TreeEntry {
                mode: "100644".into(),
                name: b"f".to_vec(),
                oid: blob,
            }],
        };
        tree.canonicalize();
        let tree_oid = repo.hash_object(ObjectType::Tree, &tree.serialize(), true).unwrap();
        let sig = crate::Signature::parse("A <a@b.c> 1700000000 +0000", "commit").unwrap();
        let commit = repo
            .commit_tree(tree_oid, Vec::new(), sig.clone(), sig, b"c\n", None)
            .unwrap();

        let send = reachable_objects(&repo, &[commit], &[]).unwrap();
        assert!(send.contains(&commit));
        assert!(send.contains(&tree_oid));
        assert!(send.contains(&blob));
        assert_eq!(send.len(), 3);

        // With the commit already had, nothing new is sent.
        let none = reachable_objects(&repo, &[commit], &[commit]).unwrap();
        assert!(none.is_empty());
    }
}
