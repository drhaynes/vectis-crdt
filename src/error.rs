use crate::types::OpId;

/// All errors that can occur in vectis-crdt operations.
#[derive(Debug, Clone)]
pub enum VectisError {
    /// The referenced stroke does not exist in the document.
    StrokeNotFound(OpId),

    /// Binary encoding failed.
    EncodingError(String),

    /// Binary decoding failed — truncated or malformed bytes.
    DecodingError(String),

    /// Actor ID is invalid (0 is reserved).
    InvalidActorId,

    /// Snapshot binary format version is not supported.
    SnapshotVersionMismatch { expected: u8, got: u8 },

    /// The causal buffer exceeded its maximum capacity.
    /// Indicates a broken connection or malicious peer.
    CausalBufferOverflow { capacity: usize },

    /// An operation or payload exceeded a configured safety limit.
    /// Prevents resource exhaustion from malformed or malicious data.
    LimitExceeded {
        /// Human-readable name of the limit that was hit.
        what: &'static str,
        /// The configured maximum.
        limit: usize,
        /// The actual value that exceeded the limit.
        actual: usize,
    },
}

impl std::fmt::Display for VectisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VectisError::StrokeNotFound(id) => {
                write!(
                    f,
                    "stroke not found: (lamport={}, actor={})",
                    id.lamport.0, id.actor.0
                )
            }
            VectisError::EncodingError(s) => write!(f, "encoding error: {s}"),
            VectisError::DecodingError(s) => write!(f, "decoding error: {s}"),
            VectisError::InvalidActorId => write!(f, "actor ID 0 is reserved"),
            VectisError::SnapshotVersionMismatch { expected, got } => write!(
                f,
                "snapshot version mismatch: expected {expected}, got {got}"
            ),
            VectisError::CausalBufferOverflow { capacity } => {
                write!(f, "causal buffer overflow at capacity {capacity}")
            }
            VectisError::LimitExceeded {
                what,
                limit,
                actual,
            } => write!(f, "limit exceeded: {what} (limit={limit}, actual={actual})"),
        }
    }
}

impl std::error::Error for VectisError {}

/// Convenience alias.
pub type VectisResult<T> = Result<T, VectisError>;
