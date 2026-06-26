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
