//! Signed policy bundles (FORNX-118, epic FORNX-69).
//!
//! **Trust boundary.** `fornax-core` (this crate, and every crate that
//! depends on it) only ever *verifies* a bundle produced elsewhere
//! (`fornax-cloud`, out of scope here). There is no `SigningKey`, no
//! `Signer` import, and no key-generation code anywhere in this module's
//! non-test paths — [`verify_bundle`] is a read-only operation over
//! untrusted bytes. See the module-level "Deviation" note below on exactly
//! how far `ed25519-dalek` itself enforces that boundary at the dependency
//! level, and how far it does not.
//!
//! **Sign the transmitted bytes verbatim.** [`SignedPolicyBundle::payload_b64`]
//! is the exact base64 the signature covers. `verify_bundle` never
//! re-serializes a parsed [`BundlePayload`] to reconstruct the signed
//! bytes — Python (`fornax-cloud`) is the producer, Rust is only ever the
//! verifier; re-serializing risks a `json.dumps`-vs-`serde_json` byte
//! divergence that would silently break verification (or worse, silently
//! accept a payload the signer never actually signed).
//!
//! **Domain separation.** The signed message is
//! [`BUNDLE_SIGNING_DOMAIN`] concatenated with the raw decoded payload
//! bytes — never the payload alone. See `docs/adr/0007-signed-policy-bundles.md`.
//!
//! **Verify-then-parse.** The envelope ([`SignedPolicyBundle`]) is
//! intentionally minimal — no nested types, no timestamps read before a
//! signature has been checked. [`MAX_PAYLOAD_BYTES`]/[`MAX_SIGNATURES`]
//! bound the pre-authentication work an attacker can force onto this path.
//!
//! **Fail-closed trust, unlike ADR-0006's fail-open selectors.** An
//! unrecognized [`SignatureAlgorithm`] is *always* a rejection here. This
//! is a deliberate inversion of `docs/adr/0006-policy-as-data.md`'s
//! "an unrecognized selector value still matches" rule: policy
//! *application* fails open (an admin policy you can't fully parse should
//! still apply, rather than silently vanish); a trust decision fails
//! closed (an algorithm this binary doesn't recognize must never be
//! treated as an accepted signature).

use std::collections::BTreeMap;

use base64::alphabet;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::DecodePaddingMode;
use base64::Engine;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::diagnostics::PolicyValidationReport;
use super::revision::PublishedPolicyRevision;
use super::target::{BoundRevision, PolicyBinding};

pub const BUNDLE_SCHEMA_VERSION: u32 = 1;
pub const SUPPORTED_BUNDLE_SCHEMA_VERSIONS: &[u32] = &[1];
pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
pub const MAX_SIGNATURES: usize = 8;
pub const BUNDLE_SIGNING_DOMAIN: &[u8] = b"fornax-policy-bundle/v1\n";
pub const CLOCK_SKEW_TOLERANCE_SECONDS: i64 = 300;

/// Strict canonical base64: standard alphabet, required padding, no
/// trailing bits. Used for both the envelope's `payload_b64` and each
/// [`TrustedKey::public_key_b64`]/[`BundleSignature::signature_b64`] — an
/// attacker-controlled payload must decode exactly one way or be rejected,
/// never be interpreted leniently.
fn strict_base64() -> GeneralPurpose {
    GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::RequireCanonical)
            .with_decode_allow_trailing_bits(false),
    )
}

/// Newtype over a trust-store key identifier. Distinct from any digest or
/// revision id — a `KeyId` names a verification key, never signed content.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct KeyId(pub String);

/// Closed for anything this binary can act on, but never rejects unknown
/// wire values at parse time — `Unrecognized` carries them forward so
/// [`verify_bundle`] can reject with the specific
/// [`BundleRejection::UnsupportedAlgorithm`] variant instead of a generic
/// parse failure. This is the trust-fails-closed inversion of ADR-0006's
/// selector rule: the *value* is accepted structurally so a precise
/// rejection can be produced, but it can never be treated as a match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureAlgorithm {
    Ed25519,
    #[serde(untagged)]
    Unrecognized(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleSignature {
    pub key_id: KeyId,
    pub algorithm: SignatureAlgorithm,
    pub signature_b64: String,
}

/// Wire/envelope form. Deliberately minimal and a plain `Deserialize` —
/// this type asserts nothing about its own validity. All authentication
/// and semantic checks happen in [`verify_bundle`], never here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedPolicyBundle {
    pub bundle_schema_version: u32,
    pub payload_b64: String,
    pub signatures: Vec<BundleSignature>,
}

/// `"sha256:<hex>"` over the exact signed payload bytes. Distinct from
/// [`super::revision::RevisionDigest`]: this digests opaque transmitted
/// bytes, not a typed, re-serializable body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadDigest(String);

impl PayloadDigest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PayloadDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn digest_payload_bytes(bytes: &[u8]) -> PayloadDigest {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(bytes);
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    PayloadDigest(format!("sha256:{hex}"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleProvenance {
    pub issuer: String,
    pub audit_ref: Option<String>,
    pub authorized_by: Option<String>,
}

/// The authenticated content of a bundle, parsed only *after* signature
/// verification succeeds (see [`verify_bundle`]'s evaluation order).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundlePayload {
    pub bundle_schema_version: u32,
    pub bundle_id: Uuid,
    /// Strictly increasing per issuer. **Not checked by [`verify_bundle`]** —
    /// rejecting a stale-but-validly-signed `sequence` requires a
    /// last-known-good comparison point that only a future
    /// activation/cache ticket (FORNX-119) has. See
    /// `docs/adr/0007-signed-policy-bundles.md`'s residual-risks section.
    pub sequence: u64,
    pub issued_at: String,
    pub not_before: String,
    pub expires_at: String,
    pub provenance: BundleProvenance,
    pub revision: PublishedPolicyRevision,
    pub bindings: Vec<PolicyBinding>,
}

/// Verified, authenticated policy bundle. Private fields, accessors only —
/// [`verify_bundle`] is the sole constructor, mirroring
/// [`PublishedPolicyRevision`]'s discipline: this type cannot be conjured
/// from untrusted input via `Deserialize`, because it has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPolicyBundle {
    payload: BundlePayload,
    verified_by: KeyId,
    payload_digest: PayloadDigest,
}

impl VerifiedPolicyBundle {
    pub fn payload(&self) -> &BundlePayload {
        &self.payload
    }

    pub fn revision(&self) -> &PublishedPolicyRevision {
        &self.payload.revision
    }

    pub fn bindings(&self) -> &[PolicyBinding] {
        &self.payload.bindings
    }

    pub fn verified_by(&self) -> &KeyId {
        &self.verified_by
    }

    pub fn payload_digest(&self) -> &PayloadDigest {
        &self.payload_digest
    }

    /// Joins every binding in this bundle with its (already digest-matched)
    /// revision via [`BoundRevision::new`]. The digest match is guaranteed
    /// by [`verify_bundle`]'s step 10 before this type can exist, so that
    /// specific failure mode of `BoundRevision::new` can never trigger
    /// here — but `BoundRevision::new` also rejects a pin declared at
    /// `TargetLevel::LocalUser` (`PinAtLocalUserLayer`), which
    /// `verify_bundle` never checks, so this can genuinely return `Err` for
    /// that reason. `BoundRevision::new` is the sole authority on both
    /// invariants; nothing here duplicates or re-derives them.
    pub fn into_bound_revisions(self) -> Result<Vec<BoundRevision>, PolicyValidationReport> {
        let revision = self.payload.revision;
        self.payload
            .bindings
            .into_iter()
            .map(|binding| BoundRevision::new(binding, revision.clone()))
            .collect()
    }
}

/// One entry in a [`TrustedVerificationKeys`] store. Trust roots are static
/// configuration (compiled-in default, operator file, env override) — this
/// type is never fetched from a network by anything in this crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedKey {
    pub key_id: KeyId,
    pub algorithm: SignatureAlgorithm,
    /// 32 raw Ed25519 public-key bytes, strict canonical base64.
    pub public_key_b64: String,
    pub not_before: Option<String>,
    pub not_after: Option<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedVerificationKeys {
    pub schema_version: u32,
    pub keys: Vec<TrustedKey>,
}

impl TrustedVerificationKeys {
    /// Rejects duplicate `key_id`s (with differing material) and malformed
    /// key bytes up front, rather than deferring either failure into
    /// [`verify_bundle`]'s per-signature loop.
    pub fn load(raw: &str) -> Result<Self, TrustStoreError> {
        let parsed: TrustedVerificationKeys =
            serde_json::from_str(raw).map_err(|e| TrustStoreError::Malformed {
                detail: e.to_string(),
            })?;

        if parsed.keys.is_empty() {
            return Err(TrustStoreError::Empty);
        }

        let mut seen: BTreeMap<&KeyId, &TrustedKey> = BTreeMap::new();
        for key in &parsed.keys {
            if let Some(existing) = seen.get(&key.key_id) {
                if *existing != key {
                    return Err(TrustStoreError::DuplicateKeyId {
                        key_id: key.key_id.clone(),
                    });
                }
            } else {
                seen.insert(&key.key_id, key);
            }

            decode_verifying_key(key).map_err(|_| TrustStoreError::MalformedKey {
                key_id: key.key_id.clone(),
            })?;

            for (label, value) in [
                ("not_before", &key.not_before),
                ("not_after", &key.not_after),
            ] {
                if let Some(v) = value {
                    if DateTime::parse_from_rfc3339(v).is_err() {
                        return Err(TrustStoreError::Malformed {
                            detail: format!(
                                "key_id {:?} has a malformed {label} timestamp: {v:?}",
                                key.key_id
                            ),
                        });
                    }
                }
            }
        }

        Ok(parsed)
    }

    pub fn get(&self, key_id: &KeyId) -> Option<&TrustedKey> {
        self.keys.iter().find(|k| &k.key_id == key_id)
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum TrustStoreError {
    #[error("trust store is malformed: {detail}")]
    Malformed { detail: String },
    #[error("key_id {key_id:?} appears more than once with differing key material")]
    DuplicateKeyId { key_id: KeyId },
    #[error("key_id {key_id:?} has malformed key bytes")]
    MalformedKey { key_id: KeyId },
    #[error("trust store contains no keys")]
    Empty,
}

fn decode_verifying_key(key: &TrustedKey) -> Result<VerifyingKey, ()> {
    let bytes = strict_base64()
        .decode(&key.public_key_b64)
        .map_err(|_| ())?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| ())?;
    VerifyingKey::from_bytes(&arr).map_err(|_| ())
}
