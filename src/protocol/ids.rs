//! 11-bit standard CAN ID encoding used by the bootloader protocol.
//!
//! The ID carries three fields packed into 11 bits:
//!
//! ```text
//!   bits 10:8  message type (3 bits)
//!   bits  7:4  source node ID (4 bits; 0x0 = host)
//!   bits  3:0  destination node ID (4 bits; 0xF = broadcast)
//! ```
//!
//! Every bootloader on the bus uses the same layout; the 16 possible
//! node IDs are assigned per-board via provisioning. Encoding is
//! network byte order on the CAN bus (big-endian within the 11 bits,
//! though at this granularity the concept is moot — it's just a bit
//! field).

use super::ParseError;

/// Message type, encoded in bits 10:8 of the 11-bit standard ID.
///
/// Values match `bl_proto_type_t` in the bootloader. 0x5 and 0x6 are
/// reserved by the spec and never emitted today; parse paths reject
/// them as unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    /// Host → device command frame.
    Cmd = 0x0,
    /// Device → host positive acknowledgement.
    Ack = 0x1,
    /// Device → host negative acknowledgement (carries a NACK code).
    Nack = 0x2,
    /// Bidirectional multi-frame payload continuation (ISO-TP CF /
    /// FC frames ride with this type regardless of the top-level
    /// command type).
    Data = 0x3,
    /// Device → host unsolicited event (heartbeat, DTC, log, live
    /// data).
    Notify = 0x4,
    /// Broadcast discovery ping (host→`0xF`) or reply (device→host).
    Discover = 0x7,
}

impl MessageType {
    /// Parse the message-type nibble straight out of the 11-bit ID.
    /// Returns `Err` for the two reserved values (0x5, 0x6) and any
    /// out-of-range input.
    pub fn from_bits(bits: u8) -> Result<Self, ParseError> {
        match bits {
            0x0 => Ok(Self::Cmd),
            0x1 => Ok(Self::Ack),
            0x2 => Ok(Self::Nack),
            0x3 => Ok(Self::Data),
            0x4 => Ok(Self::Notify),
            0x7 => Ok(Self::Discover),
            other => Err(ParseError::UnknownMessageType(other)),
        }
    }

    /// 3-bit integer value, suitable for placing into bits 10:8 of
    /// the standard ID.
    pub fn as_bits(self) -> u8 {
        self as u8
    }
}

/// Reserved node ID for the host. Always 0x0.
pub const HOST_NODE_ID: u8 = 0x0;

/// Reserved node ID for broadcast addressing. Always 0xF.
pub const BROADCAST_NODE_ID: u8 = 0xF;

// ---- Bit-field masks (mirror the BL_PROTO_* constants) ----

const TYPE_SHIFT: u8 = 8;
const SRC_SHIFT: u8 = 4;
const DST_SHIFT: u8 = 0;

const TYPE_MASK: u16 = 0x7;
const NODE_MASK: u16 = 0xF;

/// Decoded form of the 11-bit frame ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameId {
    pub message_type: MessageType,
    pub src: u8,
    pub dst: u8,
}

impl FrameId {
    /// Build a frame ID from its components. Returns an error if any
    /// node ID doesn't fit in 4 bits.
    pub fn new(message_type: MessageType, src: u8, dst: u8) -> Result<Self, ParseError> {
        if u16::from(src) > NODE_MASK {
            return Err(ParseError::NodeIdOutOfRange(src));
        }
        if u16::from(dst) > NODE_MASK {
            return Err(ParseError::NodeIdOutOfRange(dst));
        }
        Ok(Self {
            message_type,
            src,
            dst,
        })
    }

    /// Convenience: frame addressed to `dst` from the host.
    pub fn from_host(message_type: MessageType, dst: u8) -> Result<Self, ParseError> {
        Self::new(message_type, HOST_NODE_ID, dst)
    }

    /// Encode into the 11-bit standard identifier.
    pub fn encode(self) -> u16 {
        (u16::from(self.message_type.as_bits()) << TYPE_SHIFT)
            | ((u16::from(self.src) & NODE_MASK) << SRC_SHIFT)
            | ((u16::from(self.dst) & NODE_MASK) << DST_SHIFT)
    }

    /// Parse an 11-bit ID into its components. Rejects IDs with bits
    /// set above bit 10 and reserved message-type values.
    pub fn decode(raw: u16) -> Result<Self, ParseError> {
        if raw >> 11 != 0 {
            return Err(ParseError::IdOutOfRange(raw));
        }
        let type_bits = ((raw >> TYPE_SHIFT) & TYPE_MASK) as u8;
        let src = ((raw >> SRC_SHIFT) & NODE_MASK) as u8;
        let dst = ((raw >> DST_SHIFT) & NODE_MASK) as u8;
        Ok(Self {
            message_type: MessageType::from_bits(type_bits)?,
            src,
            dst,
        })
    }

    /// Is the frame addressed to this node? `self.dst == node_id` or
    /// `self.dst == BROADCAST_NODE_ID`.
    pub fn addressed_to(&self, node_id: u8) -> bool {
        self.dst == node_id || self.dst == BROADCAST_NODE_ID
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_matches_bit_layout() {
        // CMD (0x0) from host (0x0) to node 0x3 → binary 000 0000 0011 = 0x003.
        assert_eq!(
            FrameId::new(MessageType::Cmd, 0x0, 0x3).unwrap().encode(),
            0x003
        );
        // NACK (0x2) from node 0x3 to host (0x0) → 010 0011 0000 = 0x230.
        assert_eq!(
            FrameId::new(MessageType::Nack, 0x3, 0x0).unwrap().encode(),
            0x230
        );
        // DISCOVER (0x7) broadcast → 111 0000 1111 = 0x70F.
        assert_eq!(
            FrameId::from_host(MessageType::Discover, BROADCAST_NODE_ID)
                .unwrap()
                .encode(),
            0x70F
        );
    }

    #[test]
    fn decode_is_inverse_of_encode() {
        for &mt in &[
            MessageType::Cmd,
            MessageType::Ack,
            MessageType::Nack,
            MessageType::Data,
            MessageType::Notify,
            MessageType::Discover,
        ] {
            for src in 0..=0xF {
                for dst in 0..=0xF {
                    let id = FrameId::new(mt, src, dst).unwrap();
                    let round = FrameId::decode(id.encode()).unwrap();
                    assert_eq!(round, id);
                }
            }
        }
    }

    #[test]
    fn decode_rejects_reserved_types() {
        // Type bits 0x5 and 0x6 are reserved.
        let id_reserved_5 = (0x5u16 << TYPE_SHIFT) | 0x0F;
        let id_reserved_6 = (0x6u16 << TYPE_SHIFT) | 0x0F;
        assert!(matches!(
            FrameId::decode(id_reserved_5),
            Err(ParseError::UnknownMessageType(0x5))
        ));
        assert!(matches!(
            FrameId::decode(id_reserved_6),
            Err(ParseError::UnknownMessageType(0x6))
        ));
    }

    #[test]
    fn decode_rejects_out_of_range_id() {
        // Bit 11 set.
        assert!(matches!(
            FrameId::decode(0x800),
            Err(ParseError::IdOutOfRange(0x800))
        ));
    }

    #[test]
    fn new_rejects_oversize_node_ids() {
        assert!(matches!(
            FrameId::new(MessageType::Cmd, 0x10, 0x00),
            Err(ParseError::NodeIdOutOfRange(0x10))
        ));
        assert!(matches!(
            FrameId::new(MessageType::Cmd, 0x00, 0xFF),
            Err(ParseError::NodeIdOutOfRange(0xFF))
        ));
    }

    #[test]
    fn addressed_to_catches_broadcast_and_unicast() {
        let my_id = 0x3;
        let unicast = FrameId::from_host(MessageType::Cmd, my_id).unwrap();
        let broadcast = FrameId::from_host(MessageType::Discover, BROADCAST_NODE_ID).unwrap();
        let other = FrameId::from_host(MessageType::Cmd, 0x4).unwrap();
        assert!(unicast.addressed_to(my_id));
        assert!(broadcast.addressed_to(my_id));
        assert!(!other.addressed_to(my_id));
    }
}
