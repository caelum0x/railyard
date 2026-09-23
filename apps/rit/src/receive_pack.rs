//! Executable receive-pack application boundary.
//!
//! This module composes the strict wire parser with raw/thin pack quarantine and
//! one-ref publication. It is transport-agnostic: an SSH or HTTP adapter can
//! pass the request body here without gaining direct access to the target ODB or
//! refs. Multi-ref commands and packed-ref deletion fail closed until their
//! storage transactions are implemented.

use crate::error::{ForgeError, Result};
use crate::receive::{
    AtomicRefUpdate, ReceiveAtomicOutcome, ReceiveLimits, ReceiveOutcome, ReceiveTransaction,
};
use crate::refs::{AtomicRefError, RefChange, RefCommand};
use crate::wire::{
    encode_packet_line, encode_receive_status, parse_receive_pack_request, PacketLine,
    PacketLineLimits, ReceivePackCommand, ReceivePackRequest, ReceiveStatus,
};
use crate::Repository;

/// Capabilities implemented by the SHA-1 receive server. Every push is applied
/// as one all-or-nothing transaction, so `atomic` is advertised and honored for
/// multi-ref pushes. Unsupported features (push-options, sideband) are
/// intentionally absent so stock Git will not negotiate them.
pub const RECEIVE_PACK_CAPABILITIES: &[&str] = &[
    "report-status",
    "report-status-v2",
    "delete-refs",
    "atomic",
    "ofs-delta",
    "object-format=sha1",
    "agent=rit/0.1",
];

/// Server-side policy which is never inferred from client capabilities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReceivePackPolicy {
    /// Allow a branch update whose expected old commit is not an ancestor of
    /// the proposed new commit. This is a repository-owner decision, not a
    /// capability the pushing client can grant itself.
    pub allow_non_fast_forward: bool,
}

/// Result suitable for an SSH/HTTP transport adapter.
#[derive(Debug)]
pub struct ReceivePackExecution {
    /// Protocol report-status bytes, or an empty body when the client did not
    /// negotiate report-status/report-status-v2.
    pub response: Vec<u8>,
    /// Present only after objects and the destination ref were published.
    pub outcome: Option<ReceiveOutcome>,
    /// Present only after a loose ref was deleted by expected-old CAS.
    pub deleted: Option<DeletedRef>,
    /// Typed local diagnostic retained for logs/metrics. The response uses a
    /// stable, privacy-safe reason instead of exposing filesystem details.
    pub rejection: Option<ForgeError>,
}

/// Successful single-ref deletion summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeletedRef {
    pub name: String,
    pub old: crate::ObjectId,
}

/// Encode protocol-v0 receive-pack ref advertisement for a stateful SSH/local
/// process transport. The first advertised ref carries capabilities. An empty
/// repository uses Git's conventional zero-id `capabilities^{}` pseudo-ref.
pub fn advertise_receive_pack(repository: &Repository, capabilities: &[&str]) -> Result<Vec<u8>> {
    for capability in capabilities {
        if capability.is_empty() || capability.bytes().any(|byte| byte <= b' ' || byte == 0x7f) {
            return Err(ForgeError::ReceiveRejected(
                "receive advertisement contains an invalid capability".into(),
            ));
        }
    }

    let mut refs = Vec::new();
    if let Ok(head) = repository.refs().resolve_head() {
        refs.push(("HEAD".to_string(), head));
    }
    refs.extend(repository.refs().list()?);
    refs.sort_by(|left, right| left.0.cmp(&right.0));
    refs.dedup_by(|left, right| left.0 == right.0);

    let capability_list = capabilities.join(" ");
    let mut out = Vec::new();
    if refs.is_empty() {
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

/// Execute one complete receive-pack request body.
///
/// Syntactically invalid command framing returns `Err` because no trustworthy
/// ref/status vocabulary can be recovered. Valid requests rejected during
/// unpack or ref policy return `Ok(ReceivePackExecution)` with a standard
/// report-status response and a typed local `rejection`.
pub fn execute_receive_pack(
    repository: &Repository,
    input: &[u8],
    advertised_capabilities: &[&str],
    packet_limits: PacketLineLimits,
    receive_limits: ReceiveLimits,
    policy: ReceivePackPolicy,
) -> Result<ReceivePackExecution> {
    let request = parse_receive_pack_request(input, advertised_capabilities, packet_limits)?;
    let report_status = request.capabilities.contains("report-status")
        || request.capabilities.contains("report-status-v2");

    // Whether any command carries objects (create/update) and therefore expects
    // a packfile. A push made up solely of deletions carries no pack.
    let carries_objects = request
        .commands
        .iter()
        .any(|command| !matches!(command, ReceivePackCommand::Delete { .. }));

    if carries_objects {
        execute_with_pack(
            repository,
            input,
            &request,
            report_status,
            receive_limits,
            policy,
        )
    } else {
        execute_delete_only(repository, input, &request, report_status)
    }
}

/// Apply a push that stages a pack (any mix of create/update, optionally with
/// deletions) as one atomic multi-ref transaction.
fn execute_with_pack(
    repository: &Repository,
    input: &[u8],
    request: &ReceivePackRequest,
    report_status: bool,
    receive_limits: ReceiveLimits,
    policy: ReceivePackPolicy,
) -> Result<ReceivePackExecution> {
    let mut transaction = ReceiveTransaction::begin(repository, receive_limits)?;
    if let Err(error) = transaction.stage_pack(&input[request.pack_offset..]) {
        return rejected_refs(
            request,
            report_status,
            Some("pack rejected"),
            "unpacker error",
            error,
        );
    }

    let updates: Vec<AtomicRefUpdate> = request
        .commands
        .iter()
        .map(|command| atomic_update_from_command(command, policy))
        .collect();

    match transaction.finish_atomic(updates) {
        Ok(outcome) => Ok(execution_from_atomic(request, report_status, outcome)?),
        Err(error) => {
            // A whole-graph failure (bad connectivity, disconnected objects):
            // report the same stable reason on every ref, as the single-ref
            // path did, and never move a ref.
            let public_reason = public_ref_rejection(&error);
            rejected_refs(request, report_status, None, public_reason, error)
        }
    }
}

/// Apply a deletion-only push (no pack) as one atomic ref transaction.
fn execute_delete_only(
    repository: &Repository,
    input: &[u8],
    request: &ReceivePackRequest,
    report_status: bool,
) -> Result<ReceivePackExecution> {
    if request.pack_offset != input.len() {
        return rejected_refs(
            request,
            report_status,
            None,
            "unexpected pack for deletion",
            ForgeError::ReceiveRejected(
                "deletion-only request carried unexpected pack bytes".into(),
            ),
        );
    }

    let commands: Vec<RefCommand> = request
        .commands
        .iter()
        .map(|command| match command {
            ReceivePackCommand::Delete { old, name } => RefCommand {
                name: name.clone(),
                change: RefChange::Delete { old: *old },
            },
            // `carries_objects` guaranteed every command here is a deletion.
            other => unreachable!("non-delete command in deletion-only push: {other:?}"),
        })
        .collect();

    match repository.refs().atomic_update(&commands) {
        Ok(()) => {
            let statuses = request
                .commands
                .iter()
                .map(|command| ReceiveStatus {
                    ref_name: command.name().to_string(),
                    error: None,
                })
                .collect::<Vec<_>>();
            Ok(ReceivePackExecution {
                response: encode_status_body(report_status, None, &statuses)?,
                outcome: None,
                deleted: single_deleted_ref(&commands),
                rejection: None,
            })
        }
        Err(error) => atomic_failure_execution(request, report_status, error),
    }
}

/// Translate one wire command into an [`AtomicRefUpdate`], applying the server's
/// non-fast-forward policy (never inferred from the client).
fn atomic_update_from_command(
    command: &ReceivePackCommand,
    policy: ReceivePackPolicy,
) -> AtomicRefUpdate {
    match command {
        ReceivePackCommand::Create { new, name } => AtomicRefUpdate {
            name: name.clone(),
            expected_old: None,
            new: Some(*new),
            allow_non_fast_forward: policy.allow_non_fast_forward,
        },
        ReceivePackCommand::Update { old, new, name } => AtomicRefUpdate {
            name: name.clone(),
            expected_old: Some(*old),
            new: Some(*new),
            allow_non_fast_forward: policy.allow_non_fast_forward,
        },
        ReceivePackCommand::Delete { old, name } => AtomicRefUpdate {
            name: name.clone(),
            expected_old: Some(*old),
            new: None,
            allow_non_fast_forward: policy.allow_non_fast_forward,
        },
    }
}

/// Build the transport execution from a finished atomic outcome, encoding the
/// per-ref report-status and surfacing a typed rejection when nothing committed.
fn execution_from_atomic(
    request: &ReceivePackRequest,
    report_status: bool,
    outcome: ReceiveAtomicOutcome,
) -> Result<ReceivePackExecution> {
    let statuses = outcome
        .results
        .iter()
        .map(|result| ReceiveStatus {
            ref_name: result.name.clone(),
            error: result.error.clone(),
        })
        .collect::<Vec<_>>();
    let response = encode_status_body(report_status, None, &statuses)?;

    if outcome.committed {
        Ok(ReceivePackExecution {
            response,
            outcome: single_receive_outcome(request, &outcome),
            deleted: None,
            rejection: None,
        })
    } else {
        let reason = outcome
            .results
            .iter()
            .find_map(|result| result.error.clone())
            .unwrap_or_else(|| "receive rejected".into());
        Ok(ReceivePackExecution {
            response,
            outcome: None,
            deleted: None,
            rejection: Some(ForgeError::ReceiveRejected(reason)),
        })
    }
}

/// Preserve the single-ref `outcome` summary for the common one-ref update push.
fn single_receive_outcome(
    request: &ReceivePackRequest,
    outcome: &ReceiveAtomicOutcome,
) -> Option<ReceiveOutcome> {
    let [command] = request.commands.as_slice() else {
        return None;
    };
    match command {
        ReceivePackCommand::Create { new, name } => Some(ReceiveOutcome {
            ref_name: name.clone(),
            old: None,
            new: *new,
            published_objects: outcome.published_objects,
        }),
        ReceivePackCommand::Update { old, new, name } => Some(ReceiveOutcome {
            ref_name: name.clone(),
            old: Some(*old),
            new: *new,
            published_objects: outcome.published_objects,
        }),
        ReceivePackCommand::Delete { .. } => None,
    }
}

/// Preserve the single-ref `deleted` summary for the common one-ref delete push.
fn single_deleted_ref(commands: &[RefCommand]) -> Option<DeletedRef> {
    let [command] = commands else {
        return None;
    };
    match &command.change {
        RefChange::Delete { old } => Some(DeletedRef {
            name: command.name.clone(),
            old: *old,
        }),
        RefChange::Update { .. } => None,
    }
}

/// Map a rejected deletion-only atomic transaction into per-ref `ng` statuses.
fn atomic_failure_execution(
    request: &ReceivePackRequest,
    report_status: bool,
    error: AtomicRefError,
) -> Result<ReceivePackExecution> {
    let reason = public_ref_rejection(&error.source);
    let statuses = request
        .commands
        .iter()
        .map(|command| ReceiveStatus {
            ref_name: command.name().to_string(),
            error: Some(if command.name() == error.name {
                reason.to_string()
            } else {
                "atomic transaction failed".to_string()
            }),
        })
        .collect::<Vec<_>>();
    Ok(ReceivePackExecution {
        response: encode_status_body(report_status, None, &statuses)?,
        outcome: None,
        deleted: None,
        rejection: Some(error.source),
    })
}

/// Encode a report-status body, or an empty body when the client did not
/// negotiate report-status/report-status-v2.
fn encode_status_body(
    report_status: bool,
    unpack_error: Option<&str>,
    statuses: &[ReceiveStatus],
) -> Result<Vec<u8>> {
    if !report_status {
        return Ok(Vec::new());
    }
    encode_receive_status(unpack_error, statuses)
}

fn rejected_refs(
    request: &ReceivePackRequest,
    report_status: bool,
    unpack_error: Option<&str>,
    reason: &str,
    rejection: ForgeError,
) -> Result<ReceivePackExecution> {
    Ok(ReceivePackExecution {
        response: status_response(request, report_status, unpack_error, Some(reason))?,
        outcome: None,
        deleted: None,
        rejection: Some(rejection),
    })
}

fn status_response(
    request: &ReceivePackRequest,
    report_status: bool,
    unpack_error: Option<&str>,
    ref_error: Option<&str>,
) -> Result<Vec<u8>> {
    if !report_status {
        return Ok(Vec::new());
    }
    let statuses = request
        .commands
        .iter()
        .map(|command| ReceiveStatus {
            ref_name: command.name().to_string(),
            error: ref_error.map(str::to_string),
        })
        .collect::<Vec<_>>();
    encode_receive_status(unpack_error, &statuses)
}

fn public_ref_rejection(error: &ForgeError) -> &'static str {
    match error {
        ForgeError::NonFastForward { .. } => "non-fast-forward",
        ForgeError::RefConflict { .. } => "stale info",
        ForgeError::RefLocked(_) => "ref locked",
        ForgeError::Connectivity(_) | ForgeError::HashMismatch { .. } => "invalid object graph",
        ForgeError::ReceiveRejected(_) => "receive policy rejected",
        ForgeError::ResourceLimit(_) => "resource limit",
        _ => "server error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{decode_packet_lines, encode_packet_line, PacketLine};

    fn command_line(old: &str, new: &str, name: &str, capabilities: &str) -> Vec<u8> {
        encode_packet_line(&PacketLine::Data(
            format!("{old} {new} {name}\0{capabilities}\n").into_bytes(),
        ))
        .unwrap()
    }

    #[test]
    fn multi_ref_push_with_invalid_pack_rejects_every_ref_atomically() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("objects")).unwrap();
        let repo = Repository::open_git_dir(dir.path()).unwrap();
        let zero = "0000000000000000000000000000000000000000";
        let new = "1111111111111111111111111111111111111111";
        let mut input = command_line(
            zero,
            new,
            "refs/heads/main",
            "report-status-v2 object-format=sha1",
        );
        input.extend(
            encode_packet_line(&PacketLine::Data(
                format!("{zero} {new} refs/heads/topic\n").into_bytes(),
            ))
            .unwrap(),
        );
        input.extend_from_slice(b"0000not-a-pack");

        let execution = execute_receive_pack(
            &repo,
            &input,
            &["report-status-v2", "object-format=sha1"],
            PacketLineLimits::default(),
            ReceiveLimits::default(),
            ReceivePackPolicy::default(),
        )
        .unwrap();
        // Multi-ref is now processed atomically, but an undecodable pack rejects
        // the whole batch: no ref moves and every ref reports `ng`.
        assert!(execution.outcome.is_none());
        assert!(matches!(execution.rejection, Some(ForgeError::Pack(_))));
        let status = decode_packet_lines(&execution.response, PacketLineLimits::default()).unwrap();
        assert_eq!(status.len(), 4);
        assert!(
            matches!(&status[1], PacketLine::Data(line) if line.starts_with(b"ng refs/heads/main"))
        );
        assert!(
            matches!(&status[2], PacketLine::Data(line) if line.starts_with(b"ng refs/heads/topic"))
        );
    }

    #[test]
    fn omits_status_body_when_client_did_not_negotiate_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("objects")).unwrap();
        let repo = Repository::open_git_dir(dir.path()).unwrap();
        let zero = "0000000000000000000000000000000000000000";
        let new = "1111111111111111111111111111111111111111";
        let mut input = command_line(zero, new, "refs/heads/main", "object-format=sha1");
        input.extend_from_slice(b"0000not-a-pack");
        let execution = execute_receive_pack(
            &repo,
            &input,
            &["object-format=sha1"],
            PacketLineLimits::default(),
            ReceiveLimits::default(),
            ReceivePackPolicy::default(),
        )
        .unwrap();
        assert!(execution.response.is_empty());
        assert!(execution.rejection.is_some());
    }

    #[test]
    fn advertises_empty_and_populated_repositories_without_unsupported_caps() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("objects")).unwrap();
        std::fs::create_dir_all(dir.path().join("refs/heads")).unwrap();
        let repo = Repository::open_git_dir(dir.path()).unwrap();
        let empty = advertise_receive_pack(&repo, RECEIVE_PACK_CAPABILITIES).unwrap();
        let packets = decode_packet_lines(&empty, PacketLineLimits::default()).unwrap();
        assert!(
            matches!(&packets[0], PacketLine::Data(line) if line.starts_with(b"0000000000000000000000000000000000000000 capabilities^{}\0"))
        );
        // Atomic multi-ref receive is now advertised and honored.
        assert!(empty
            .windows(b"atomic".len())
            .any(|window| window == b"atomic"));
        assert!(empty
            .windows(b"delete-refs".len())
            .any(|window| window == b"delete-refs"));

        let oid = crate::hash_content(crate::ObjectType::Blob, b"advertised").0;
        repo.refs()
            .compare_and_swap("refs/heads/main", None, oid)
            .unwrap();
        let populated = advertise_receive_pack(&repo, RECEIVE_PACK_CAPABILITIES).unwrap();
        let packets = decode_packet_lines(&populated, PacketLineLimits::default()).unwrap();
        let expected = format!("{oid} refs/heads/main");
        assert!(packets.iter().any(
            |packet| matches!(packet, PacketLine::Data(line) if line.starts_with(expected.as_bytes()))
        ));
    }

    #[test]
    fn executes_single_loose_ref_deletion_without_a_pack() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("objects")).unwrap();
        std::fs::create_dir_all(dir.path().join("refs/heads")).unwrap();
        let repo = Repository::open_git_dir(dir.path()).unwrap();
        let old = "1111111111111111111111111111111111111111";
        std::fs::write(dir.path().join("refs/heads/main"), format!("{old}\n")).unwrap();
        let zero = "0000000000000000000000000000000000000000";
        let mut input = command_line(
            old,
            zero,
            "refs/heads/main",
            "report-status-v2 object-format=sha1",
        );
        input.extend_from_slice(b"0000");

        let execution = execute_receive_pack(
            &repo,
            &input,
            RECEIVE_PACK_CAPABILITIES,
            PacketLineLimits::default(),
            ReceiveLimits::default(),
            ReceivePackPolicy::default(),
        )
        .unwrap();
        assert!(execution.outcome.is_none());
        assert_eq!(
            execution.deleted,
            Some(DeletedRef {
                name: "refs/heads/main".into(),
                old: crate::ObjectId::from_hex(old).unwrap(),
            })
        );
        assert!(execution.rejection.is_none());
        assert!(repo.refs().resolve("refs/heads/main").is_err());
        let status = decode_packet_lines(&execution.response, PacketLineLimits::default()).unwrap();
        assert!(matches!(&status[1], PacketLine::Data(line) if line == b"ok refs/heads/main\n"));
    }
}
