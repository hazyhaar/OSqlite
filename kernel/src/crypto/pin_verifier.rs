/// SPKI certificate pin management for TLS connections.
///
/// ## Current limitation
///
/// embedded-tls 0.18 marks `CertificateRef.entries` as `pub(crate)`, which
/// prevents external code from inspecting the server certificate inside
/// `TlsVerifier::verify_certificate`. Full SPKI pin verification requires:
///   (a) upstream patch to make `entries` pub, or
///   (b) enabling the `rustpki` feature (requires std/webpki)
///
/// This module provides the pin storage infrastructure (`pin set/show/clear`
/// shell commands) and the SHA-256 helper, ready for when cert access is
/// available. The `ENFORCE_PINNING` flag in `api/mod.rs` is set to `false`
/// until the upstream limitation is resolved.

use spin::Mutex;

// ============================================================
// Runtime pin storage (for `pin set` shell command)
// ============================================================

/// Runtime pin override. When set, will be used for pin verification
/// once the embedded-tls cert access limitation is resolved.
static PIN_OVERRIDE: Mutex<Option<[u8; 32]>> = Mutex::new(None);

/// Set a runtime SPKI pin override (from `pin set <hex>` shell command).
pub fn set_pin_override(hash: [u8; 32]) {
    *PIN_OVERRIDE.lock() = Some(hash);
}

/// Clear the runtime pin override.
pub fn clear_pin_override() {
    *PIN_OVERRIDE.lock() = None;
}

/// Get the current pin override, if any.
pub fn get_pin_override() -> Option<[u8; 32]> {
    *PIN_OVERRIDE.lock()
}

/// Compute the SHA-256 hash of raw bytes.
///
/// Used by the `pin` shell command to compute SPKI hashes, and will be
/// used by the pin verifier once cert access is available.
pub fn sha256_hash(data: &[u8]) -> [u8; 32] {
    use sha2::{Sha256, Digest};
    let hash = Sha256::digest(data);
    let mut result = [0u8; 32];
    result.copy_from_slice(hash.as_slice());
    result
}
