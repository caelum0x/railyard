//! Strict Git pkt-line framing and receive-pack command parsing.
//!
//! Pkt-line is the shared binary framing used by Git's pack protocols and
//! protocol v2. Push itself still arrives as receive-pack ref-update commands
//! followed by raw pack data; this module intentionally follows that documented
//! boundary instead of inventing a proprietary "v2 push" command.

use std::collections::BTreeSet;

use crate::error::{ForgeError, Result};
use crate::oid::ObjectId;
use crate::refs::validate_refname;

pub const MAX_PACKET_PAYLOAD: usize = 65_516;
pub const MAX_PACKET_LENGTH: usize = MAX_PACKET_PAYLOAD + 4;
const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// One decoded pkt-line control or binary data frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PacketLine {
    Data(Vec<u8>),
    Flush,
    Delimiter,
    ResponseEnd,
}

/// Aggregate parser limits beyond Git's per-packet 65,516-byte ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PacketLineLimits {
    pub max_packets: usize,
    pub max_total_payload: usize,
}

impl Default for PacketLineLimits {
    fn default() -> Self {
        Self {
            max_packets: 100_000,
            max_total_payload: 64 * 1024 * 1024,
        }
    }
}

/// One create/update/delete command decoded from receive-pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceivePackCommand {
    Create {
        new: ObjectId,
        name: String,
    },
    Update {
        old: ObjectId,
        new: ObjectId,
        name: String,
    },
    Delete {
        old: ObjectId,
        name: String,
    },
}

impl ReceivePackCommand {
    pub fn name(&self) -> &str {
        match self {
            Self::Create { name, .. } | Self::Update { name, .. } | Self::Delete { name, .. } => {
                name
            }
        }
    }
}

/// Parsed ref commands and the offset at which raw pack bytes begin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceivePackRequest {
    pub commands: Vec<ReceivePackCommand>,
    pub capabilities: BTreeSet<String>,
    pub pack_offset: usize,
}

/// One receive-pack report-status result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiveStatus {
    pub ref_name: String,
    pub error: Option<String>,
}

/// Encode one packet, including special control frames.
pub fn encode_packet_line(packet: &PacketLine) -> Result<Vec<u8>> {
    match packet {
        PacketLine::Flush => Ok(b"0000".to_vec()),
        PacketLine::Delimiter => Ok(b"0001".to_vec()),
        PacketLine::ResponseEnd => Ok(b"0002".to_vec()),
        PacketLine::Data(payload) => {
            if payload.len() > MAX_PACKET_PAYLOAD {
                return Err(ForgeError::ResourceLimit(format!(
                    "pkt-line payload exceeds {MAX_PACKET_PAYLOAD} bytes"
                )));
            }
            let total = payload.len() + 4;
            let mut encoded = format!("{total:04x}").into_bytes();
            encoded.extend_from_slice(payload);
            Ok(encoded)
        }
    }
}

/// Decode an entire pkt-line buffer and reject trailing/truncated bytes.
pub fn decode_packet_lines(input: &[u8], limits: PacketLineLimits) -> Result<Vec<PacketLine>> {
    let mut offset = 0;
    let mut payload_bytes = 0usize;
    let mut packets = Vec::new();
    while offset < input.len() {
        if packets.len() == limits.max_packets {
            return Err(ForgeError::ResourceLimit(format!(
                "pkt-line stream exceeds {} packets",
                limits.max_packets
            )));
        }
        let (packet, consumed) = decode_one(&input[offset..])?;
        if let PacketLine::Data(payload) = &packet {
            payload_bytes = payload_bytes
                .checked_add(payload.len())
                .ok_or_else(|| ForgeError::ResourceLimit("pkt-line byte count overflow".into()))?;
            if payload_bytes > limits.max_total_payload {
                return Err(ForgeError::ResourceLimit(format!(
                    "pkt-line stream exceeds {} payload bytes",
                    limits.max_total_payload
                )));
            }
        }
        packets.push(packet);
        offset += consumed;
    }
    Ok(packets)
}

/// Parse receive-pack's command pkt-lines and return the raw pack boundary.
/// Client capabilities are accepted only when the caller advertised them.
pub fn parse_receive_pack_request(
    input: &[u8],
    advertised_capabilities: &[&str],
    limits: PacketLineLimits,
) -> Result<ReceivePackRequest> {
    let mut offset = 0usize;
    let mut payload_bytes = 0usize;
    let mut commands = Vec::new();
    let mut capabilities = BTreeSet::new();
    let advertised = advertised_capabilities
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();

    loop {
        let (packet, consumed) = decode_one(input.get(offset..).unwrap_or_default())?;
        offset += consumed;
        match packet {
            PacketLine::Flush => break,
            PacketLine::Data(mut line) => {
                if commands.len() == limits.max_packets {
                    return Err(ForgeError::ResourceLimit(format!(
                        "receive-pack exceeds {} commands",
                        limits.max_packets
                    )));
                }
                payload_bytes = payload_bytes.checked_add(line.len()).ok_or_else(|| {
                    ForgeError::ResourceLimit("receive command byte count overflow".into())
                })?;
                if payload_bytes > limits.max_total_payload {
                    return Err(ForgeError::ResourceLimit(
                        "receive commands exceed payload limit".into(),
                    ));
                }
                if line.last() == Some(&b'\n') {
                    line.pop();
                }
                let command_bytes = if commands.is_empty() {
                    let (command, requested) = split_first_capabilities(&line)?;
                    for capability in requested {
                        if !capability_was_advertised(&capability, &advertised) {
                            return Err(ForgeError::ReceiveRejected(format!(
                                "client requested unadvertised capability {capability}"
                            )));
                        }
                        capabilities.insert(capability);
                    }
                    command
                } else {
                    if line.contains(&0) {
                        return Err(ForgeError::ReceiveRejected(
                            "only the first receive command may carry capabilities".into(),
                        ));
                    }
                    line.as_slice()
                };
                commands.push(parse_receive_command(command_bytes)?);
            }
            PacketLine::Delimiter | PacketLine::ResponseEnd => {
                return Err(ForgeError::ReceiveRejected(
                    "receive-pack command list contains a protocol-v2 control packet".into(),
                ));
            }
        }
    }

    if commands.is_empty() {
        return Err(ForgeError::ReceiveRejected(
            "receive-pack command list is empty".into(),
        ));
    }
    let mut names = BTreeSet::new();
    for command in &commands {
        if !names.insert(command.name()) {
            return Err(ForgeError::ReceiveRejected(format!(
                "receive-pack updates {} more than once",
                command.name()
            )));
        }
        if matches!(command, ReceivePackCommand::Delete { .. })
            && !advertised.contains("delete-refs")
        {
            return Err(ForgeError::ReceiveRejected(
                "delete command requires the server to advertise delete-refs".into(),
            ));
        }
    }
    Ok(ReceivePackRequest {
        commands,
        capabilities,
        pack_offset: offset,
    })
}

/// Encode the standard report-status response after unpack/ref processing.
pub fn encode_receive_status(
    unpack_error: Option<&str>,
    statuses: &[ReceiveStatus],
) -> Result<Vec<u8>> {
    if statuses.is_empty() {
        return Err(ForgeError::ReceiveRejected(
            "receive status requires at least one ref result".into(),
        ));
    }
    let mut out = Vec::new();
    let unpack = match unpack_error {
        Some(error) => {
            validate_status_error(error)?;
            format!("unpack {error}\n")
        }
        None => "unpack ok\n".to_string(),
    };
    out.extend(encode_packet_line(&PacketLine::Data(unpack.into_bytes()))?);
    for status in statuses {
        validate_refname(&status.ref_name)?;
        let line = match status.error.as_deref() {
            Some(error) => {
                validate_status_error(error)?;
                format!("ng {} {error}\n", status.ref_name)
            }
            None => format!("ok {}\n", status.ref_name),
        };
        out.extend(encode_packet_line(&PacketLine::Data(line.into_bytes()))?);
    }
    out.extend_from_slice(b"0000");
    Ok(out)
}

fn validate_status_error(error: &str) -> Result<()> {
    if error.is_empty() || error == "ok" || error.contains(['\0', '\n', '\r']) {
        return Err(ForgeError::ReceiveRejected(
            "receive status contains an invalid error message".into(),
        ));
    }
    Ok(())
}

fn decode_one(input: &[u8]) -> Result<(PacketLine, usize)> {
    if input.len() < 4 {
        return Err(ForgeError::ReceiveRejected(
            "truncated pkt-line length".into(),
        ));
    }
    let header = std::str::from_utf8(&input[..4])
        .map_err(|_| ForgeError::ReceiveRejected("pkt-line length is not ASCII".into()))?;
    if !header.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ForgeError::ReceiveRejected(
            "pkt-line length is not hexadecimal".into(),
        ));
    }
    let length = usize::from_str_radix(header, 16)
        .map_err(|_| ForgeError::ReceiveRejected("invalid pkt-line length".into()))?;
    match length {
        0 => Ok((PacketLine::Flush, 4)),
        1 => Ok((PacketLine::Delimiter, 4)),
        2 => Ok((PacketLine::ResponseEnd, 4)),
        3 => Err(ForgeError::ReceiveRejected(
            "reserved pkt-line length 0003".into(),
        )),
        4..=MAX_PACKET_LENGTH => {
            if input.len() < length {
                return Err(ForgeError::ReceiveRejected(format!(
                    "truncated pkt-line: declared {length} bytes, received {}",
                    input.len()
                )));
            }
            Ok((PacketLine::Data(input[4..length].to_vec()), length))
        }
        _ => Err(ForgeError::ResourceLimit(format!(
            "pkt-line length exceeds {MAX_PACKET_LENGTH} bytes"
        ))),
    }
}

fn split_first_capabilities(line: &[u8]) -> Result<(&[u8], Vec<String>)> {
    let Some(nul) = line.iter().position(|byte| *byte == 0) else {
        return Ok((line, Vec::new()));
    };
    let command = &line[..nul];
    let capabilities = std::str::from_utf8(&line[nul + 1..])
        .map_err(|_| ForgeError::ReceiveRejected("capability list is not UTF-8".into()))?
        .split_whitespace()
        .map(str::to_string)
        .collect();
    Ok((command, capabilities))
}

fn capability_was_advertised(capability: &str, advertised: &BTreeSet<&str>) -> bool {
    if advertised.contains(capability) {
        return true;
    }
    let name = capability
        .split_once('=')
        .map_or(capability, |(name, _)| name);
    // Agent values identify each endpoint independently. Other valued
    // capabilities (notably object-format) must match exactly; accepting a
    // value merely because its key was advertised could negotiate semantics the
    // server cannot execute.
    name == "agent"
        && advertised.iter().any(|candidate| {
            candidate == &"agent"
                || candidate
                    .split_once('=')
                    .is_some_and(|(key, _)| key == "agent")
        })
}

fn parse_receive_command(line: &[u8]) -> Result<ReceivePackCommand> {
    let line = std::str::from_utf8(line)
        .map_err(|_| ForgeError::ReceiveRejected("receive command is not UTF-8".into()))?;
    let fields = line.split(' ').collect::<Vec<_>>();
    if fields.len() != 3 || fields.iter().any(|field| field.is_empty()) {
        return Err(ForgeError::ReceiveRejected(
            "receive command must be '<old> <new> <ref>'".into(),
        ));
    }
    let old = fields[0];
    let new = fields[1];
    let name = fields[2].to_string();
    validate_refname(&name)?;
    match (old == ZERO_OID, new == ZERO_OID) {
        (true, true) => Err(ForgeError::ReceiveRejected(
            "receive command cannot create and delete simultaneously".into(),
        )),
        (true, false) => Ok(ReceivePackCommand::Create {
            new: ObjectId::from_hex(new)?,
            name,
        }),
        (false, true) => Ok(ReceivePackCommand::Delete {
            old: ObjectId::from_hex(old)?,
            name,
        }),
        (false, false) => Ok(ReceivePackCommand::Update {
            old: ObjectId::from_hex(old)?,
            new: ObjectId::from_hex(new)?,
            name,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(byte: char) -> String {
        std::iter::repeat(byte).take(40).collect()
    }

    #[test]
    fn pkt_line_is_binary_safe_and_handles_v2_controls() {
        let packets = vec![
            PacketLine::Data(vec![0, 1, b'\n', 255]),
            PacketLine::Delimiter,
            PacketLine::ResponseEnd,
            PacketLine::Flush,
        ];
        let encoded = packets
            .iter()
            .map(encode_packet_line)
            .collect::<Result<Vec<_>>>()
            .unwrap()
            .concat();
        assert_eq!(
            decode_packet_lines(&encoded, PacketLineLimits::default()).unwrap(),
            packets
        );
    }

    #[test]
    fn pkt_line_rejects_truncation_reserved_and_oversize_frames() {
        for invalid in [&b"000"[..], &b"0003"[..], &b"0008abc"[..], &b"zzzz"[..]] {
            assert!(decode_packet_lines(invalid, PacketLineLimits::default()).is_err());
        }
        assert!(encode_packet_line(&PacketLine::Data(vec![0; MAX_PACKET_PAYLOAD + 1])).is_err());
    }

    #[test]
    fn parses_receive_commands_capabilities_and_pack_boundary() {
        let old = oid('1');
        let new = oid('2');
        let first = format!("{old} {new} refs/heads/main\0report-status-v2 agent=rit-test\n");
        let mut input = encode_packet_line(&PacketLine::Data(first.into_bytes())).unwrap();
        input.extend_from_slice(b"0000PACKraw-data");

        let parsed = parse_receive_pack_request(
            &input,
            &["report-status-v2", "agent=rit/0.1"],
            PacketLineLimits {
                max_packets: 1,
                max_total_payload: 1024,
            },
        )
        .unwrap();
        assert_eq!(parsed.pack_offset, input.len() - b"PACKraw-data".len());
        assert_eq!(&input[parsed.pack_offset..], b"PACKraw-data");
        assert!(parsed.capabilities.contains("report-status-v2"));
        assert!(matches!(
            &parsed.commands[0],
            ReceivePackCommand::Update { name, .. } if name == "refs/heads/main"
        ));
    }

    #[test]
    fn rejects_unadvertised_capability_duplicate_ref_and_unsafe_delete() {
        let old = oid('1');
        let new = oid('2');
        let line = format!("{old} {new} refs/heads/main\0atomic\n");
        let mut input = encode_packet_line(&PacketLine::Data(line.into_bytes())).unwrap();
        input.extend_from_slice(b"0000");
        assert!(parse_receive_pack_request(
            &input,
            &["report-status-v2"],
            PacketLineLimits::default()
        )
        .is_err());

        let wrong_format = format!("{old} {new} refs/heads/main\0object-format=sha256\n");
        let mut wrong_format_input =
            encode_packet_line(&PacketLine::Data(wrong_format.into_bytes())).unwrap();
        wrong_format_input.extend_from_slice(b"0000");
        assert!(parse_receive_pack_request(
            &wrong_format_input,
            &["object-format=sha1"],
            PacketLineLimits::default()
        )
        .is_err());

        let first = format!("{old} {new} refs/heads/main\n");
        let second = format!("{new} {old} refs/heads/main\n");
        let mut duplicate = encode_packet_line(&PacketLine::Data(first.into_bytes())).unwrap();
        duplicate.extend(encode_packet_line(&PacketLine::Data(second.into_bytes())).unwrap());
        duplicate.extend_from_slice(b"0000");
        assert!(parse_receive_pack_request(&duplicate, &[], PacketLineLimits::default()).is_err());

        let delete = format!("{old} {ZERO_OID} refs/heads/main\n");
        let mut delete_input = encode_packet_line(&PacketLine::Data(delete.into_bytes())).unwrap();
        delete_input.extend_from_slice(b"0000");
        assert!(
            parse_receive_pack_request(&delete_input, &[], PacketLineLimits::default()).is_err()
        );
        assert!(matches!(
            &parse_receive_pack_request(
                &delete_input,
                &["delete-refs"],
                PacketLineLimits::default()
            )
            .unwrap()
            .commands[0],
            ReceivePackCommand::Delete { name, .. } if name == "refs/heads/main"
        ));
    }

    #[test]
    fn status_response_roundtrips_through_pkt_line_parser() {
        let encoded = encode_receive_status(
            None,
            &[
                ReceiveStatus {
                    ref_name: "refs/heads/main".into(),
                    error: None,
                },
                ReceiveStatus {
                    ref_name: "refs/heads/topic".into(),
                    error: Some("non-fast-forward".into()),
                },
            ],
        )
        .unwrap();
        let decoded = decode_packet_lines(&encoded, PacketLineLimits::default()).unwrap();
        assert_eq!(decoded.last(), Some(&PacketLine::Flush));
        assert_eq!(decoded.len(), 4);
        assert!(encode_receive_status(None, &[]).is_err());
        assert!(encode_receive_status(
            Some("bad\nstatus"),
            &[ReceiveStatus {
                ref_name: "refs/heads/main".into(),
                error: None,
            }]
        )
        .is_err());
    }
}
