//! Response parser — turns a completed ISO-TP reassembly + the
//! decoded message-type byte into a typed [`Response`] enum.
//!
//! The adapter layer reassembles each ISO-TP message into a
//! `Vec<u8>`, extracts the message-type byte (payload byte 1, after
//! the PCI — see `protocol::ids` for the wire format), and hands the
//! pair here along with the *remaining* bytes (opcode + args).
//!
//! Parse rules:
//!
//! - `Cmd` / `DiscoverRequest` / `AppCtrl` are never received by the
//!   host, only sent — [`parse`] returns an `Invalid` error to make
//!   it loud rather than silent.
//! - `Ack` — byte 0 is the echoed opcode, rest is opcode-specific
//!   payload. We don't interpret the payload; callers downcast by
//!   opcode.
//! - `Nack` — byte 0 is the rejected opcode, byte 1 is a [`NackCode`]
//!   (lenient, unknown bytes become `Unknown(u8)`).
//! - `Notify` — byte 0 is the notify opcode (see [`NotifyOpcode`]),
//!   rest is opcode-specific payload.
//! - `DiscoverReply` — byte 0 is `CMD_DISCOVER = 0x03`, byte 1 = node
//!   ID, bytes 2..3 = protocol (major, minor).

use super::ids::MessageType;
use super::opcodes::NackCode;
use super::ParseError;

/// Every shape of payload the host might receive after ISO-TP
/// reassembly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    /// Positive acknowledgement. `opcode` is the command being
    /// acknowledged; `payload` is whatever bytes followed it
    /// (length depends on the opcode).
    Ack { opcode: u8, payload: Vec<u8> },

    /// Negative acknowledgement. `rejected_opcode` is the one the
    /// device refused; `code` is the [`NackCode`].
    Nack { rejected_opcode: u8, code: NackCode },

    /// Unsolicited notification. `opcode` is the `NotifyOpcode` byte
    /// (e.g. `0xF0` heartbeat, `0xF3` live data); `payload` is
    /// whatever bytes followed it.
    Notify { opcode: u8, payload: Vec<u8> },

    /// Discover reply. Always 4 bytes on the wire:
    /// `[CMD_DISCOVER, node_id, major, minor]`.
    Discover {
        node_id: u8,
        proto_major: u8,
        proto_minor: u8,
    },
}

impl Response {
    /// Parse a completed ISO-TP message. `message_type` is the type
    /// nibble from the 11-bit CAN ID; `bytes` is the reassembled
    /// payload (without any ISO-TP framing).
    pub fn parse(message_type: MessageType, bytes: &[u8]) -> Result<Self, ParseError> {
        match message_type {
            MessageType::Ack => {
                if bytes.is_empty() {
                    return Err(ParseError::Invalid("empty ACK payload"));
                }
                Ok(Self::Ack {
                    opcode: bytes[0],
                    payload: bytes[1..].to_vec(),
                })
            }
            MessageType::Nack => {
                if bytes.len() < 2 {
                    return Err(ParseError::Invalid("NACK shorter than 2 bytes"));
                }
                Ok(Self::Nack {
                    rejected_opcode: bytes[0],
                    code: NackCode::from_byte(bytes[1]),
                })
            }
            MessageType::Notify => {
                if bytes.is_empty() {
                    return Err(ParseError::Invalid("empty NOTIFY payload"));
                }
                Ok(Self::Notify {
                    opcode: bytes[0],
                    payload: bytes[1..].to_vec(),
                })
            }
            MessageType::DiscoverReply => {
                if bytes.len() < 4 {
                    return Err(ParseError::Invalid("DISCOVER reply shorter than 4 bytes"));
                }
                // First byte should be CMD_DISCOVER; we don't crash if
                // it isn't (future spec could repurpose), but keep the
                // fields caller-visible.
                Ok(Self::Discover {
                    node_id: bytes[1],
                    proto_major: bytes[2],
                    proto_minor: bytes[3],
                })
            }
            MessageType::Cmd => Err(ParseError::Invalid(
                "CMD frame received at host — bootloaders don't send CMDs",
            )),
            MessageType::DiscoverRequest => Err(ParseError::Invalid(
                "DISCOVER_REQUEST frame received at host — only the host sends these",
            )),
            MessageType::AppCtrl => Err(ParseError::Invalid(
                "APP_CTRL frame received at host — app-control traffic is host-to-node only",
            )),
        }
    }

    /// Short `Ack | Nack | Notify | Discover` label for logs.
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Ack { .. } => "ACK",
            Self::Nack { .. } => "NACK",
            Self::Notify { .. } => "NOTIFY",
            Self::Discover { .. } => "DISCOVER",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::opcodes::NotifyOpcode;

    #[test]
    fn parse_ack_with_payload() {
        let bytes = vec![0x05, 0xAA, 0xBB, 0xCC];
        match Response::parse(MessageType::Ack, &bytes).unwrap() {
            Response::Ack { opcode, payload } => {
                assert_eq!(opcode, 0x05);
                assert_eq!(payload, vec![0xAA, 0xBB, 0xCC]);
            }
            other => panic!("expected Ack, got {other:?}"),
        }
    }

    #[test]
    fn parse_nack_known_code() {
        let bytes = vec![0x10, 0x01]; // FLASH_ERASE rejected with PROTECTED_ADDR
        match Response::parse(MessageType::Nack, &bytes).unwrap() {
            Response::Nack {
                rejected_opcode,
                code,
            } => {
                assert_eq!(rejected_opcode, 0x10);
                assert_eq!(code, NackCode::ProtectedAddr);
            }
            other => panic!("expected Nack, got {other:?}"),
        }
    }

    #[test]
    fn parse_nack_unknown_code_is_lenient() {
        let bytes = vec![0xFF, 0x42];
        match Response::parse(MessageType::Nack, &bytes).unwrap() {
            Response::Nack { code, .. } => assert_eq!(code, NackCode::Unknown(0x42)),
            other => panic!("expected Nack, got {other:?}"),
        }
    }

    #[test]
    fn parse_notify_heartbeat_shape() {
        // [opcode=0xF0, node_id, reset_cause, flags_low, uptime_le24]
        let bytes = vec![
            NotifyOpcode::Heartbeat.as_byte(),
            3,
            1,
            0x13,
            0x10,
            0x00,
            0x00,
        ];
        match Response::parse(MessageType::Notify, &bytes).unwrap() {
            Response::Notify { opcode, payload } => {
                assert_eq!(opcode, 0xF0);
                assert_eq!(payload, vec![3, 1, 0x13, 0x10, 0x00, 0x00]);
            }
            other => panic!("expected Notify, got {other:?}"),
        }
    }

    #[test]
    fn parse_discover_reply() {
        let bytes = vec![0x03, 0x03, 0x00, 0x01];
        match Response::parse(MessageType::DiscoverReply, &bytes).unwrap() {
            Response::Discover {
                node_id,
                proto_major,
                proto_minor,
            } => {
                assert_eq!(node_id, 0x03);
                assert_eq!(proto_major, 0x00);
                assert_eq!(proto_minor, 0x01);
            }
            other => panic!("expected Discover, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_cmd_at_host() {
        let err = Response::parse(MessageType::Cmd, &[0x01, 0, 1]).unwrap_err();
        assert!(matches!(err, ParseError::Invalid(_)));
    }

    #[test]
    fn parse_rejects_discover_request_at_host() {
        let err = Response::parse(MessageType::DiscoverRequest, &[0x03]).unwrap_err();
        assert!(matches!(err, ParseError::Invalid(_)));
    }

    #[test]
    fn parse_rejects_app_ctrl_at_host() {
        let err = Response::parse(MessageType::AppCtrl, &[0x01]).unwrap_err();
        assert!(matches!(err, ParseError::Invalid(_)));
    }

    #[test]
    fn parse_rejects_short_ack() {
        let err = Response::parse(MessageType::Ack, &[]).unwrap_err();
        assert!(matches!(err, ParseError::Invalid(_)));
    }

    #[test]
    fn parse_rejects_short_nack() {
        let err = Response::parse(MessageType::Nack, &[0x01]).unwrap_err();
        assert!(matches!(err, ParseError::Invalid(_)));
    }

    #[test]
    fn parse_rejects_short_discover() {
        let err = Response::parse(MessageType::DiscoverReply, &[0x03, 0x01, 0x00]).unwrap_err();
        assert!(matches!(err, ParseError::Invalid(_)));
    }
}
