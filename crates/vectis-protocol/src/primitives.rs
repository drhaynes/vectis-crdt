use crate::error::{ProtocolError, ProtocolResult};

pub(crate) fn encode_string(value: &str, out: &mut Vec<u8>) {
    encode_bytes(value.as_bytes(), out);
}

pub(crate) fn decode_string_at(bytes: &[u8], cursor: &mut usize) -> ProtocolResult<String> {
    let raw = decode_bytes_at(bytes, cursor)?;
    String::from_utf8(raw).map_err(|_| ProtocolError::InvalidUtf8)
}

pub(crate) fn encode_bytes(value: &[u8], out: &mut Vec<u8>) {
    encode_varint(value.len() as u64, out);
    out.extend_from_slice(value);
}

pub(crate) fn decode_bytes_at(bytes: &[u8], cursor: &mut usize) -> ProtocolResult<Vec<u8>> {
    let len = decode_varint_at(bytes, cursor)? as usize;
    let end = cursor.checked_add(len).ok_or(ProtocolError::Truncated)?;
    if end > bytes.len() {
        return Err(ProtocolError::Truncated);
    }
    let value = bytes[*cursor..end].to_vec();
    *cursor = end;
    Ok(value)
}

pub(crate) fn encode_varint(mut value: u64, out: &mut Vec<u8>) {
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

pub(crate) fn decode_varint_at(bytes: &[u8], cursor: &mut usize) -> ProtocolResult<u64> {
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
