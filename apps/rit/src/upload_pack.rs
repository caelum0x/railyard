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
//!   emitted either **full** or as `OFS_DELTA` entries against a nearby base
//!   inside a bounded window (see [`generate_pack_with_options`]); either shape
//!   is a valid, self-contained pack that `git index-pack`/`git unpack-objects`
//!   decode and `git fsck` validates.
//!
//! [`upload_pack_v0`] ties them together for the stateless-RPC (HTTP) case:
//! parse the request, compute the set, and return `NAK` + the pack.

use std::collections::{HashMap, HashSet, VecDeque};

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
/// `ofs-delta` is advertised because [`generate_pack`] may emit `OFS_DELTA`
/// entries; the offsets are always intra-pack and self-contained (never thin).
pub const UPLOAD_PACK_CAPABILITIES: &[&str] = &["agent=rit/0.1", "object-format=sha1", "ofs-delta"];

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
    let mut caps: Vec<String> = UPLOAD_PACK_CAPABILITIES
        .iter()
        .map(|c| c.to_string())
        .collect();
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
            return Err(ForgeError::ResourceLimit(
                "fetch object walk too large".into(),
            ));
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
            return Err(ForgeError::ResourceLimit(
                "fetch tree walk too large".into(),
            ));
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

/// The pack object type code for the `OFS_DELTA` representation.
const OFS_DELTA: u8 = 6;

/// Tuning for [`generate_pack_with_options`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackOptions {
    /// How many preceding candidates (in the type/size-sorted order) each object
    /// may be deltified against. `0` disables deltification entirely.
    pub window: usize,
    /// Hard cap on the length of any single `OFS_DELTA` base chain. Bounds the
    /// work a reader must do to reconstruct one object and keeps our own encoder
    /// from stacking pathologically deep chains.
    pub max_chain_depth: usize,
}

impl Default for PackOptions {
    fn default() -> Self {
        // A window of 10 matches Git's default `pack.window`.
        Self {
            window: 10,
            max_chain_depth: 50,
        }
    }
}

/// One object's decoded bytes, kept in memory while we search for delta bases.
struct RawObject {
    object_type: ObjectType,
    payload: Vec<u8>,
}

/// Build a self-contained packfile containing exactly `oids`, using the default
/// delta [`PackOptions`]. Objects that compress better as a delta of a nearby
/// object are emitted as `OFS_DELTA`; the rest are emitted full. The result is a
/// valid non-thin pack (every delta base is included in the same pack).
pub fn generate_pack(repo: &Repository, oids: &[ObjectId]) -> Result<Vec<u8>> {
    generate_pack_with_options(repo, oids, PackOptions::default())
}

/// Build a self-contained packfile with an explicit delta [`PackOptions`].
pub fn generate_pack_with_options(
    repo: &Repository,
    oids: &[ObjectId],
    options: PackOptions,
) -> Result<Vec<u8>> {
    let count = u32::try_from(oids.len())
        .map_err(|_| ForgeError::ResourceLimit("pack object count exceeds u32".into()))?;

    // Read every object once; delta search needs the raw bytes on hand.
    let mut objects: Vec<RawObject> = Vec::with_capacity(oids.len());
    for oid in oids {
        let (object_type, payload) = repo.odb().read_raw(oid)?;
        objects.push(RawObject {
            object_type,
            payload,
        });
    }

    // Emit in a stable "delta-friendly" order: group by type, then largest
    // first so bigger objects become bases for their smaller, similar peers.
    let mut order: Vec<usize> = (0..objects.len()).collect();
    order.sort_by(|&a, &b| {
        type_code(objects[a].object_type)
            .cmp(&type_code(objects[b].object_type))
            .then_with(|| objects[b].payload.len().cmp(&objects[a].payload.len()))
            .then_with(|| oids[a].cmp(&oids[b]))
    });

    // Decide a delta base (an earlier position in `order`) for each object.
    let chosen = select_deltas(&objects, &order, options);

    let mut pack: Vec<u8> = Vec::new();
    pack.extend_from_slice(b"PACK");
    pack.extend_from_slice(&2u32.to_be_bytes());
    pack.extend_from_slice(&count.to_be_bytes());

    // Byte offset (from pack start) of each object, keyed by position in `order`,
    // so an OFS_DELTA can encode the distance back to its already-written base.
    let mut offsets = vec![0u64; order.len()];
    for position in 0..order.len() {
        offsets[position] = pack.len() as u64;
        match &chosen[position] {
            Some(delta) => {
                pack.extend(encode_object_header(OFS_DELTA, delta.instructions.len()));
                let distance = offsets[position]
                    .checked_sub(offsets[delta.base_position])
                    .ok_or_else(|| ForgeError::Pack("delta base must precede its object".into()))?;
                pack.extend(encode_ofs_delta_offset(distance));
                pack.extend(zlib_compress(&delta.instructions)?);
            }
            None => {
                let object = &objects[order[position]];
                pack.extend(encode_object_header(
                    type_code(object.object_type),
                    object.payload.len(),
                ));
                pack.extend(zlib_compress(&object.payload)?);
            }
        }
    }

    // Trailing SHA-1 over the whole pack (excluding the trailer itself).
    let mut hasher = Sha1::new();
    hasher.update(&pack);
    pack.extend_from_slice(&hasher.finalize());
    Ok(pack)
}

/// A chosen delta representation for one object at a position in the emit order.
struct ChosenDelta {
    /// Position (in `order`) of the base object, always earlier than this one.
    base_position: usize,
    /// The uncompressed copy/insert instruction stream.
    instructions: Vec<u8>,
}

/// For each object in `order`, pick the smallest delta against one of the
/// `window` preceding same-type objects, if any beats storing it full.
fn select_deltas(
    objects: &[RawObject],
    order: &[usize],
    options: PackOptions,
) -> Vec<Option<ChosenDelta>> {
    let mut chosen: Vec<Option<ChosenDelta>> = (0..order.len()).map(|_| None).collect();
    let mut depth = vec![0usize; order.len()];
    if options.window == 0 {
        return chosen;
    }

    for position in 0..order.len() {
        let target = &objects[order[position]];
        // Too small to recover the delta header overhead — always store full.
        if target.payload.len() < MIN_DELTA_TARGET {
            continue;
        }
        let window_start = position.saturating_sub(options.window);
        let mut best: Option<ChosenDelta> = None;
        for candidate in window_start..position {
            let base = &objects[order[candidate]];
            if base.object_type != target.object_type {
                continue;
            }
            if depth[candidate] >= options.max_chain_depth {
                continue;
            }
            let instructions = encode_delta(&base.payload, &target.payload);
            // Only worth it if the delta is strictly smaller than the object.
            if instructions.len() >= target.payload.len() {
                continue;
            }
            let better = best
                .as_ref()
                .map(|current| instructions.len() < current.instructions.len())
                .unwrap_or(true);
            if better {
                best = Some(ChosenDelta {
                    base_position: candidate,
                    instructions,
                });
            }
        }
        if let Some(delta) = best {
            depth[position] = depth[delta.base_position] + 1;
            chosen[position] = Some(delta);
        }
    }
    chosen
}

/// Objects below this size never delta well: two size varints plus at least one
/// instruction already rival the object, so we skip the search.
const MIN_DELTA_TARGET: usize = 32;

/// Compress `data` with zlib exactly as a pack payload requires.
fn zlib_compress(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data)?;
    encoder
        .finish()
        .map_err(|e| ForgeError::Decompress(format!("pack compression failed: {e}")))
}

/// Encode the negative base offset carried by an `OFS_DELTA` header: the byte
/// distance from the delta object back to its base, in Git's big-endian
/// base-128 form with the `-1` continuation bias.
fn encode_ofs_delta_offset(mut offset: u64) -> Vec<u8> {
    let mut buf = vec![(offset & 0x7f) as u8];
    offset >>= 7;
    while offset != 0 {
        offset -= 1;
        buf.push(0x80 | (offset & 0x7f) as u8);
        offset >>= 7;
    }
    buf.reverse();
    buf
}

/// Produce a copy/insert delta that reconstructs `target` from `base`, in Git's
/// delta instruction encoding (LE base-128 sizes, then copy/insert opcodes).
fn encode_delta(base: &[u8], target: &[u8]) -> Vec<u8> {
    const MIN_MATCH: usize = 4;
    // Cap how many base positions share one 4-gram bucket, bounding the search.
    const MAX_CHAIN: usize = 64;
    // A single copy encodes at most a 3-byte size field.
    const MAX_COPY: usize = 0x00ff_ffff;

    let mut out = Vec::new();
    encode_delta_size(&mut out, base.len());
    encode_delta_size(&mut out, target.len());

    // Index every 4-gram in the base so target windows can find copy sources.
    let mut index: HashMap<[u8; 4], Vec<usize>> = HashMap::new();
    if base.len() >= MIN_MATCH {
        for i in 0..=base.len() - MIN_MATCH {
            let key = [base[i], base[i + 1], base[i + 2], base[i + 3]];
            let bucket = index.entry(key).or_default();
            if bucket.len() < MAX_CHAIN {
                bucket.push(i);
            }
        }
    }

    let mut pending: Vec<u8> = Vec::new();
    let mut i = 0usize;
    while i < target.len() {
        let mut best_len = 0usize;
        let mut best_off = 0usize;
        if i + MIN_MATCH <= target.len() {
            let key = [target[i], target[i + 1], target[i + 2], target[i + 3]];
            if let Some(bucket) = index.get(&key) {
                for &pos in bucket {
                    let limit = MAX_COPY.min(target.len() - i).min(base.len() - pos);
                    let mut length = 0usize;
                    while length < limit && target[i + length] == base[pos + length] {
                        length += 1;
                    }
                    if length > best_len {
                        best_len = length;
                        best_off = pos;
                    }
                }
            }
        }
        if best_len >= MIN_MATCH {
            flush_insert(&mut out, &mut pending);
            emit_copy(&mut out, best_off, best_len);
            i += best_len;
        } else {
            pending.push(target[i]);
            i += 1;
            if pending.len() == 0x7f {
                flush_insert(&mut out, &mut pending);
            }
        }
    }
    flush_insert(&mut out, &mut pending);
    out
}

/// Encode an object size in the delta header's little-endian base-128 form.
fn encode_delta_size(out: &mut Vec<u8>, mut size: usize) {
    loop {
        let byte = (size & 0x7f) as u8;
        size >>= 7;
        if size == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

/// Emit any buffered literal bytes as one or more insert instructions (each
/// carries at most 127 bytes; the opcode is the length with the high bit clear).
fn flush_insert(out: &mut Vec<u8>, pending: &mut Vec<u8>) {
    for chunk in pending.chunks(0x7f) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
    pending.clear();
}

/// Emit a copy instruction: high bit set, then a presence bitmap selecting which
/// of the four offset bytes and three size bytes follow (omitting zero bytes).
fn emit_copy(out: &mut Vec<u8>, offset: usize, size: usize) {
    let mut cmd = 0x80u8;
    let mut bytes = Vec::new();
    let offset = offset as u32;
    for shift in 0..4u32 {
        let byte = (offset >> (shift * 8)) as u8;
        if byte != 0 {
            cmd |= 1 << shift;
            bytes.push(byte);
        }
    }
    let size = size as u32;
    for shift in 0..3u32 {
        let byte = (size >> (shift * 8)) as u8;
        if byte != 0 {
            cmd |= 1 << (4 + shift);
            bytes.push(byte);
        }
    }
    out.push(cmd);
    out.extend_from_slice(&bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::ObjectType;

    #[test]
    fn pack_header_and_trailer_are_wellformed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();
        let a = repo
            .hash_object(ObjectType::Blob, b"alpha\n", true)
            .unwrap();
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
        let tree_oid = repo
            .hash_object(ObjectType::Tree, &tree.serialize(), true)
            .unwrap();
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
