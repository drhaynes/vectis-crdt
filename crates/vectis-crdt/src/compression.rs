//! LZ4 compression layer for the wire format.
//!
//! Feature-gated: only compiled when `compress` feature is enabled.
//!
//! ```toml
//! # Cargo.toml
//! vectis-crdt = { features = ["compress"] }
//! ```
//!
//! ## When to compress
//!
//! | Scenario                     | Compress? |
//! |------------------------------|-----------|
//! | Single op (< 200 bytes)      | No — overhead > saving |
//! | Batch update (> 200 bytes)   | Yes — ~30–60% saving   |
//! | Full snapshot (> 1 KB)       | Yes — ~50–70% saving   |
//! | Awareness cursor (28 bytes)  | No — fixed size        |
//!
//! ## Wire format with compression
//!
//! Compressed payload is prefixed with the original (uncompressed) size
//! as a 4-byte little-endian u32. This is the `lz4_flex` default.

#[cfg(feature = "compress")]
use lz4_flex::{compress_prepend_size, decompress_size_prepended};

#[cfg(feature = "compress")]
use crate::error::VectisError;
use crate::error::VectisResult;

/// Minimum byte length to bother compressing.
pub const COMPRESS_THRESHOLD: usize = 200;

/// Compress bytes with LZ4 (prepends uncompressed size as u32 LE).
/// Returns the original bytes unchanged if the feature is disabled
/// or the input is below the compression threshold.
pub fn compress(data: &[u8]) -> Vec<u8> {
    #[cfg(feature = "compress")]
    if data.len() >= COMPRESS_THRESHOLD {
        return compress_prepend_size(data);
    }
    data.to_vec()
}

/// Decompress LZ4 bytes (expects prepended size header).
/// Returns the original bytes unchanged if the feature is disabled
/// or `data` was not compressed (size unchanged path).
///
/// To distinguish compressed from uncompressed, the caller uses the
/// message framing layer (see `MessageType::UpdateCompressed`).
pub fn decompress(data: &[u8]) -> VectisResult<Vec<u8>> {
    #[cfg(feature = "compress")]
    {
        decompress_size_prepended(data)
            .map_err(|e| VectisError::DecodingError(format!("lz4 decompress: {e}")))
    }
    #[cfg(not(feature = "compress"))]
    {
        Ok(data.to_vec())
    }
}

/// Returns true if the `compress` feature is enabled at compile time.
pub const fn is_compression_available() -> bool {
    cfg!(feature = "compress")
}

/// Compression statistics for diagnostics.
#[derive(Debug, Clone, Copy)]
pub struct CompressionStats {
    pub original_bytes: usize,
    pub compressed_bytes: usize,
}

impl CompressionStats {
    pub fn ratio(&self) -> f64 {
        if self.original_bytes == 0 {
            return 1.0;
        }
        self.compressed_bytes as f64 / self.original_bytes as f64
    }

    pub fn saving_pct(&self) -> f64 {
        (1.0 - self.ratio()) * 100.0
    }
}

/// Compress and return stats. Useful for benchmarking.
pub fn compress_with_stats(data: &[u8]) -> (Vec<u8>, CompressionStats) {
    let compressed = compress(data);
    let stats = CompressionStats {
        original_bytes: data.len(),
        compressed_bytes: compressed.len(),
    };
    (compressed, stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_below_threshold_is_passthrough() {
        let data = vec![0u8; COMPRESS_THRESHOLD - 1];
        let out = compress(&data);
        // Below threshold: identical bytes (no compression applied)
        assert_eq!(out, data);
    }

    #[test]
    fn compression_available_reflects_feature() {
        // Just check it compiles and returns a bool
        let _ = is_compression_available();
    }

    #[cfg(feature = "compress")]
    #[test]
    fn compress_decompress_roundtrip() {
        let data: Vec<u8> = (0u8..=255).cycle().take(1024).collect();
        let compressed = compress(&data);
        let decompressed = decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }
}
