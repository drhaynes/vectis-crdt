use vectis_crdt::types::ActorId;

pub const PROTOCOL_VERSION: u8 = 1;

const TAG_CLIENT_HELLO: u8 = 0x01;
const TAG_SERVER_WELCOME: u8 = 0x02;
const TAG_SNAPSHOT: u8 = 0x03;
const TAG_UPDATE: u8 = 0x04;
const TAG_STATE_VECTOR: u8 = 0x05;
const TAG_ERROR: u8 = 0x06;
const TAG_MVV: u8 = 0x07;
const TAG_AWARENESS: u8 = 0x08;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolMessage {
    ClientHello {
        room: String,
        resume_token: String,
        state_vector: Vec<u8>,
    },
    ServerWelcome {
        actor: ActorId,
        color: u32,
        resume_token: String,
    },
    Snapshot {
        bytes: Vec<u8>,
    },
    Update {
        bytes: Vec<u8>,
    },
    StateVector {
        bytes: Vec<u8>,
    },
    Mvv {
        bytes: Vec<u8>,
    },
    Awareness {
        bytes: Vec<u8>,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    Empty,
    VersionMismatch { expected: u8, got: u8 },
    UnknownTag(u8),
    Truncated,
    InvalidUtf8,
    TrailingBytes,
}

pub type ProtocolResult<T> = Result<T, ProtocolError>;

pub fn encode_message(message: &ProtocolMessage) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(PROTOCOL_VERSION);
    match message {
        ProtocolMessage::ClientHello {
            room,
            resume_token,
            state_vector,
        } => {
            out.push(TAG_CLIENT_HELLO);
            encode_string(room, &mut out);
            encode_string(resume_token, &mut out);
            encode_bytes(state_vector, &mut out);
        }
        ProtocolMessage::ServerWelcome {
            actor,
            color,
            resume_token,
        } => {
            out.push(TAG_SERVER_WELCOME);
            encode_varint(actor.0, &mut out);
            out.extend_from_slice(&color.to_le_bytes());
            encode_string(resume_token, &mut out);
        }
        ProtocolMessage::Snapshot { bytes } => {
            out.push(TAG_SNAPSHOT);
            encode_bytes(bytes, &mut out);
        }
        ProtocolMessage::Update { bytes } => {
            out.push(TAG_UPDATE);
            encode_bytes(bytes, &mut out);
        }
        ProtocolMessage::StateVector { bytes } => {
            out.push(TAG_STATE_VECTOR);
            encode_bytes(bytes, &mut out);
        }
        ProtocolMessage::Mvv { bytes } => {
            out.push(TAG_MVV);
            encode_bytes(bytes, &mut out);
        }
        ProtocolMessage::Awareness { bytes } => {
            out.push(TAG_AWARENESS);
            encode_bytes(bytes, &mut out);
        }
        ProtocolMessage::Error { message } => {
            out.push(TAG_ERROR);
            encode_string(message, &mut out);
        }
    }
    out
}

pub fn decode_message(bytes: &[u8]) -> ProtocolResult<ProtocolMessage> {
    if bytes.is_empty() {
        return Err(ProtocolError::Empty);
    }
    let version = bytes[0];
    if version != PROTOCOL_VERSION {
        return Err(ProtocolError::VersionMismatch {
            expected: PROTOCOL_VERSION,
            got: version,
        });
    }
    if bytes.len() < 2 {
        return Err(ProtocolError::Truncated);
    }

    let tag = bytes[1];
    let mut cursor = 2;
    let message = match tag {
        TAG_CLIENT_HELLO => {
            let room = decode_string_at(bytes, &mut cursor)?;
            let resume_token = decode_string_at(bytes, &mut cursor)?;
            let state_vector = decode_bytes_at(bytes, &mut cursor)?;
            ProtocolMessage::ClientHello {
                room,
                resume_token,
                state_vector,
            }
        }
        TAG_SERVER_WELCOME => {
            let actor = ActorId(decode_varint_at(bytes, &mut cursor)?);
            if cursor + 4 > bytes.len() {
                return Err(ProtocolError::Truncated);
            }
            let color = u32::from_le_bytes(
                bytes[cursor..cursor + 4]
                    .try_into()
                    .map_err(|_| ProtocolError::Truncated)?,
            );
            cursor += 4;
            let resume_token = decode_string_at(bytes, &mut cursor)?;
            ProtocolMessage::ServerWelcome {
                actor,
                color,
                resume_token,
            }
        }
        TAG_SNAPSHOT => ProtocolMessage::Snapshot {
            bytes: decode_bytes_at(bytes, &mut cursor)?,
        },
        TAG_UPDATE => ProtocolMessage::Update {
            bytes: decode_bytes_at(bytes, &mut cursor)?,
        },
        TAG_STATE_VECTOR => ProtocolMessage::StateVector {
            bytes: decode_bytes_at(bytes, &mut cursor)?,
        },
        TAG_MVV => ProtocolMessage::Mvv {
            bytes: decode_bytes_at(bytes, &mut cursor)?,
        },
        TAG_AWARENESS => ProtocolMessage::Awareness {
            bytes: decode_bytes_at(bytes, &mut cursor)?,
        },
        TAG_ERROR => ProtocolMessage::Error {
            message: decode_string_at(bytes, &mut cursor)?,
        },
        other => return Err(ProtocolError::UnknownTag(other)),
    };

    if cursor != bytes.len() {
        return Err(ProtocolError::TrailingBytes);
    }

    Ok(message)
}

fn encode_string(value: &str, out: &mut Vec<u8>) {
    encode_bytes(value.as_bytes(), out);
}

fn decode_string_at(bytes: &[u8], cursor: &mut usize) -> ProtocolResult<String> {
    let raw = decode_bytes_at(bytes, cursor)?;
    String::from_utf8(raw).map_err(|_| ProtocolError::InvalidUtf8)
}

fn encode_bytes(value: &[u8], out: &mut Vec<u8>) {
    encode_varint(value.len() as u64, out);
    out.extend_from_slice(value);
}

fn decode_bytes_at(bytes: &[u8], cursor: &mut usize) -> ProtocolResult<Vec<u8>> {
    let len = decode_varint_at(bytes, cursor)? as usize;
    let end = cursor.checked_add(len).ok_or(ProtocolError::Truncated)?;
    if end > bytes.len() {
        return Err(ProtocolError::Truncated);
    }
    let value = bytes[*cursor..end].to_vec();
    *cursor = end;
    Ok(value)
}

fn encode_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn decode_varint_at(bytes: &[u8], cursor: &mut usize) -> ProtocolResult<u64> {
    let mut result = 0u64;
    let mut shift = 0u32;
    while *cursor < bytes.len() {
        let byte = bytes[*cursor];
        *cursor += 1;
        result |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
        if shift >= 64 {
            return Err(ProtocolError::Truncated);
        }
    }
    Err(ProtocolError::Truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_hello_roundtrip() {
        let message = ProtocolMessage::ClientHello {
            room: "demo".to_string(),
            resume_token: "token-1".to_string(),
            state_vector: vec![1, 2, 3],
        };
        let encoded = encode_message(&message);
        assert_eq!(decode_message(&encoded), Ok(message));
    }

    #[test]
    fn welcome_roundtrip() {
        let message = ProtocolMessage::ServerWelcome {
            actor: ActorId(42),
            color: 0xa78bfaff,
            resume_token: "token-1".to_string(),
        };
        let encoded = encode_message(&message);
        assert_eq!(decode_message(&encoded), Ok(message));
    }

    #[test]
    fn mvv_and_awareness_roundtrip() {
        for message in [
            ProtocolMessage::Mvv {
                bytes: vec![1, 2, 3],
            },
            ProtocolMessage::Awareness {
                bytes: vec![4, 5, 6],
            },
        ] {
            let encoded = encode_message(&message);
            assert_eq!(decode_message(&encoded), Ok(message));
        }
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut encoded = encode_message(&ProtocolMessage::Update { bytes: vec![1] });
        encoded.push(9);
        assert_eq!(decode_message(&encoded), Err(ProtocolError::TrailingBytes));
    }
}
