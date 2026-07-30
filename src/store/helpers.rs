/// Helper utilities for the store module.
use chrono::DateTime;
use sha2::{Digest, Sha256};
use std::time::SystemTime;

// ── SHA-256 hashing ────────────────────────────────

/// Compute the hex-encoded SHA-256 hash of `content`.
pub fn compute_hash(content: &[u8]) -> String {
    hex::encode(Sha256::digest(content))
}

// ── Timestamp formatting ───────────────────────────

/// Format a `SystemTime` into an ISO-8601-like string (`YYYY-MM-DD HH:MM:SS`).
pub(crate) fn format_system_time(time: SystemTime) -> String {
    let secs = time
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    DateTime::from_timestamp(secs, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "1970-01-01 00:00:00".to_string())
}