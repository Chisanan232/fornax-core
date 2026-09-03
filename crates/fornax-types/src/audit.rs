//! Canonical audit event model (FORNX-314, epic FORNX-20).
//!
//! See `docs/adr/0011-audit-event-model.md` — this module is the Rust
//! mirror of that ADR's wire contract and is written to be read alongside
//! it, not in place of it. The ADR is the authoritative, self-sufficient
//! wire contract (a Python engineer on `fornax-cloud`'s side implements
//! against the ADR text alone, per ADR-0009's own precedent); this module
//! only needs to agree with it byte-for-byte.
//!
//! **Pure types and validation only.** No database table, no store
//! integration, no HTTP route, no CLI command, no network or file I/O of
//! any kind — exactly the scope FORNX-116 held for the policy model before
//! any of ADR-0007/0008/0009's storage/activation machinery existed on top
//! of it.
//!
//! # Two enum shapes, deliberately different
//!
//! - [`AuditAction`], [`AuditOutcome`], [`AuditExportClass`] are plain
//!   string enums with a `#[serde(untagged)] Unrecognized(String)` tail —
//!   the same idiom as [`crate::TrustClass`]/[`crate::CollectionMethod`]/
//!   [`crate::ContentClass`]. An unknown wire tag round-trips byte-for-byte:
//!   the original string is preserved and re-serializes to itself.
//! - [`AuditActor`] and [`AuditTarget`] are internally-tagged objects
//!   (`#[serde(tag = "actor_kind"/"target_kind")]`) with a
//!   `#[serde(other)] Unrecognized` unit-variant tail — the same idiom as
//!   [`crate::policy::RevocationTarget`]. This tail cannot
//!   preserve an unrecognized tag's associated payload (there is no payload
//!   to preserve generically once the tag itself isn't understood — see
//!   `RevocationTarget`'s own precedent), but the *event* containing it
//!   still parses successfully rather than failing outright.
//!
//! `AuditEvent` itself follows [`crate::ExtensionEnvelope`]'s
//! `#[serde(try_from = "...Wire")]` pattern: `schema_version` is checked
//! before the domain type is ever constructed, and any unrecognized
//! top-level field is preserved via `#[serde(flatten)]` rather than
//! silently dropped, so a round trip through an older or newer binary never
//! destroys data it doesn't understand.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const AUDIT_SCHEMA_VERSION: u32 = 1;
pub const SUPPORTED_AUDIT_SCHEMA_VERSIONS: &[u32] = &[1];

/// Who/what performed an audited action. See
/// `docs/adr/0011-audit-event-model.md` §2 for the full rationale,
/// including why `service_token`/`system` are new here (`fornax-cloud`'s
/// `PermissionSubjectType` enum has only `user`/`device` today).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "actor_kind", rename_all = "snake_case")]
pub enum AuditActor {
    /// Mirrors `fornax-cloud`'s `PermissionSubjectType.DEVICE`.
    Device { actor_id: String },
    /// Mirrors `fornax-cloud`'s `PermissionSubjectType.USER`.
    User { actor_id: String },
    /// A non-interactive service credential. New in this ADR — no existing
    /// subject-type vocabulary covers this caller shape.
    ServiceToken { actor_id: String },
    /// The Fornax system itself, acting with no external caller (e.g. an
    /// unattended, automatic state transition). New in this ADR, for the
    /// same reason as `ServiceToken`. Has no singular identity to name.
    System,
    /// Forward-compatibility catch-all for an `actor_kind` this binary
    /// doesn't recognize. Cannot preserve the unrecognized tag's own
    /// payload — see the module docs' "Two enum shapes" section and
    /// `RevocationTarget::Unrecognized`'s identical precedent.
    #[serde(other)]
    Unrecognized,
}

/// What an audited action was performed on. Same internally-tagged +
/// `#[serde(other)]` shape as [`AuditActor`], for the same reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target_kind", rename_all = "snake_case")]
pub enum AuditTarget {
    PolicyBundle {
        target_id: String,
    },
    RevocationEntry {
        target_id: String,
    },
    RoleAssignment {
        target_id: String,
    },
    Permission {
        target_id: String,
    },
    Device {
        target_id: String,
    },
    Organization {
        target_id: String,
    },
    #[serde(other)]
    Unrecognized,
}

/// What happened. Open string enum — see
/// `docs/adr/0011-audit-event-model.md` §2 for the canonical set and the
/// rationale for keeping it deliberately small at publication time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    PermissionCheck,
    BreakGlassActivated,
    PolicyBundleActivated,
    PolicyRevocationIngested,
    RoleAssignmentChanged,
    /// Forward-compatibility catch-all. Preserves and round-trips the
    /// original wire string verbatim — see the module docs' "Two enum
    /// shapes" section.
    #[serde(untagged)]
    Unrecognized(String),
}

/// The result of an audited action. **A strict superset of the outcome
/// strings `fornax-cloud`'s `permissions.py` already emits today** — see
/// `docs/adr/0011-audit-event-model.md` §2's table and this module's
/// `every_fornax_cloud_permission_outcome_string_has_an_audit_outcome_variant`
/// test, which asserts each literal string that file logs via `outcome=`
/// has a corresponding named variant here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    /// `fornax-cloud` `permissions.py`: `outcome=granted`.
    Granted,
    /// `fornax-cloud` `permissions.py`: `outcome=denied`.
    Denied,
    /// `fornax-cloud` `permissions.py`: `outcome=granted_via_break_glass`.
    GrantedViaBreakGlass,
    /// `fornax-cloud` `permissions.py`: `outcome=break_glass_activated`.
    BreakGlassActivated,
    /// New in this ADR — no existing `fornax-cloud` log line emits this yet.
    Revoked,
    /// New in this ADR — no existing `fornax-cloud` log line emits this yet.
    Expired,
    #[serde(untagged)]
    Unrecognized(String),
}

/// Which classification tier an [`AuditEvent`]'s own content falls into for
/// cloud-export purposes — a new, third axis alongside
/// [`crate::RetentionClass`] and [`crate::ContentClass`]. See
/// `docs/adr/0011-audit-event-model.md` §3 for the full three-axis
/// contrast and the statement that this is the last classification axis
/// this project adds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditExportClass {
    /// Only structural fields — no content that could itself be sensitive.
    /// Freely exportable.
    Metadata,
    /// A summary/verdict derived from evidence, no raw content attached.
    FindingSummary,
    /// References (ids, digests) to evidence that may itself be sensitive,
    /// without embedding the evidence content.
    SensitiveEvidenceRef,
    /// Embeds raw content directly. The most restrictive named class.
    RawContent,
    /// Forward-compatibility catch-all. See [`AuditExportClass::is_exportable`]
    /// for the fail-closed handling of this variant.
    #[serde(untagged)]
    Unrecognized(String),
}

impl AuditExportClass {
    /// Safe-default export decision (ADR-0011 §3): an unrecognized export
    /// class must be treated as maximally restrictive — as if it were
    /// [`AuditExportClass::RawContent`] — never as freely exportable. This
    /// mirrors `bundle.rs`'s fail-closed handling of an unrecognized
    /// signature algorithm (an unknown value is never silently trusted),
    /// which is the inverse of `sensor.rs`'s `CollectionMethod`/
    /// `ClockSource` "safest" default — those are honest-*unknown* markers
    /// for a pre-existing field, not a restrictiveness ordering, and
    /// `AuditExportClass` has no such predates-this-field case to honor.
    ///
    /// Only [`AuditExportClass::Metadata`] and
    /// [`AuditExportClass::FindingSummary`] are considered exportable by
    /// this coarse check; [`AuditExportClass::SensitiveEvidenceRef`],
    /// [`AuditExportClass::RawContent`], and any unrecognized value are
    /// not. This is a structural default only — actual export decisions
    /// still go through policy (ADR-0006), never through this method
    /// alone.
    pub fn is_exportable(&self) -> bool {
        matches!(
            self,
            AuditExportClass::Metadata | AuditExportClass::FindingSummary
        )
    }
}

/// One canonical audit event. See `docs/adr/0011-audit-event-model.md` §1
/// for the full field-by-field wire contract and a worked JSON example.
///
/// Deserializes through [`AuditEventWire`] (mirroring
/// [`crate::ExtensionEnvelope`]'s own `#[serde(try_from = "...")]`
/// pattern) so an unsupported `schema_version` is rejected with a specific
/// error before this type is ever constructed, while an unrecognized
/// top-level field is preserved rather than dropped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "AuditEventWire")]
pub struct AuditEvent {
    pub schema_version: u32,
    pub event_id: String,
    /// RFC3339 timestamp, matching every other timestamp field in this
    /// repo (`RevocationEntry::revoked_at`, `BundlePayload::issued_at`,
    /// etc.) — never a numeric epoch.
    pub occurred_at: String,
    pub actor: AuditActor,
    pub action: AuditAction,
    pub target: AuditTarget,
    pub outcome: AuditOutcome,
    pub export_class: AuditExportClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// Loosely-typed catch-all for action-specific detail. **Not an event
    /// lake** — see the ADR §5 and `docs/adr/0005-schema-evolution.md`'s
    /// identical non-goal for `ExtensionEnvelope::fields`, referenced here
    /// rather than re-litigated.
    #[serde(
        default = "default_attributes",
        skip_serializing_if = "is_empty_object"
    )]
    pub attributes: serde_json::Value,
    /// Any top-level JSON key not named above, preserved verbatim so a
    /// round trip through this binary never destroys what a newer producer
    /// wrote. Empty for an event constructed by this binary's own `new`.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

fn default_attributes() -> serde_json::Value {
    serde_json::json!({})
}

fn is_empty_object(v: &serde_json::Value) -> bool {
    matches!(v, serde_json::Value::Object(map) if map.is_empty())
}

impl AuditEvent {
    /// Construct a fresh event stamped with the current
    /// [`AUDIT_SCHEMA_VERSION`], no correlation id, empty `attributes`, and
    /// no unknown fields.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        event_id: impl Into<String>,
        occurred_at: impl Into<String>,
        actor: AuditActor,
        action: AuditAction,
        target: AuditTarget,
        outcome: AuditOutcome,
        export_class: AuditExportClass,
    ) -> Self {
        Self {
            schema_version: AUDIT_SCHEMA_VERSION,
            event_id: event_id.into(),
            occurred_at: occurred_at.into(),
            actor,
            action,
            target,
            outcome,
            export_class,
            correlation_id: None,
            attributes: default_attributes(),
            unknown: BTreeMap::new(),
        }
    }
}

/// Wire shape accepted on deserialization. Structurally identical to
/// [`AuditEvent`]; exists only so [`TryFrom`] can gate on `schema_version`
/// before the domain type is ever constructed — see
/// [`crate::extension::ExtensionEnvelope`]'s identical precedent.
#[derive(Debug, Deserialize)]
pub struct AuditEventWire {
    schema_version: u32,
    event_id: String,
    occurred_at: String,
    actor: AuditActor,
    action: AuditAction,
    target: AuditTarget,
    outcome: AuditOutcome,
    export_class: AuditExportClass,
    #[serde(default)]
    correlation_id: Option<String>,
    #[serde(default = "default_attributes")]
    attributes: serde_json::Value,
    #[serde(flatten)]
    unknown: BTreeMap<String, serde_json::Value>,
}

/// Exhaustive rejection vocabulary for constructing an [`AuditEvent`] from
/// untrusted wire bytes.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AuditEventRejection {
    #[error("audit_schema_version {found} is not supported (supported: {supported:?})")]
    UnsupportedSchemaVersion { found: u32, supported: Vec<u32> },
}

impl TryFrom<AuditEventWire> for AuditEvent {
    type Error = AuditEventRejection;

    fn try_from(w: AuditEventWire) -> Result<Self, Self::Error> {
        if !SUPPORTED_AUDIT_SCHEMA_VERSIONS.contains(&w.schema_version) {
            return Err(AuditEventRejection::UnsupportedSchemaVersion {
                found: w.schema_version,
                supported: SUPPORTED_AUDIT_SCHEMA_VERSIONS.to_vec(),
            });
        }
        Ok(AuditEvent {
            schema_version: w.schema_version,
            event_id: w.event_id,
            occurred_at: w.occurred_at,
            actor: w.actor,
            action: w.action,
            target: w.target,
            outcome: w.outcome,
            export_class: w.export_class,
            correlation_id: w.correlation_id,
            attributes: w.attributes,
            unknown: w.unknown,
        })
    }
}

/// Structural validation entry point for an already-deserialized
/// [`AuditEvent`] (e.g. one round-tripped from storage rather than freshly
/// parsed from wire bytes, where [`TryFrom<AuditEventWire>`] would not run
/// again). Currently only re-checks the schema version; kept as a named
/// function — mirroring `bundle.rs`/`revocation.rs`'s `verify_*` naming —
/// so future structural rules (e.g. a non-empty `event_id`) have a single
/// place to live rather than being re-derived at each call site.
pub fn validate_audit_event(event: &AuditEvent) -> Result<(), AuditEventRejection> {
    if !SUPPORTED_AUDIT_SCHEMA_VERSIONS.contains(&event.schema_version) {
        return Err(AuditEventRejection::UnsupportedSchemaVersion {
            found: event.schema_version,
            supported: SUPPORTED_AUDIT_SCHEMA_VERSIONS.to_vec(),
        });
    }
    Ok(())
}

/// Parsed form of the `<issuer-scope>:<event_id>` string
/// [`RevocationEntry::audit_ref`]-shaped fields are defined (ADR-0011 §6) to
/// resolve to. `issuer_scope` must not itself contain a `:` — parsing
/// splits on the *first* colon only.
///
/// [`RevocationEntry::audit_ref`]: crate::RevocationEntry::audit_ref
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRef {
    pub issuer_scope: String,
    pub event_id: String,
}

/// Exhaustive rejection vocabulary for [`AuditRef::parse`].
#[derive(Debug, Clone, thiserror::Error)]
pub enum AuditRefParseError {
    #[error("audit_ref {input:?} has no ':' separator between issuer-scope and event_id")]
    MissingSeparator { input: String },
    #[error("audit_ref {input:?} has an empty issuer-scope")]
    EmptyIssuerScope { input: String },
    #[error("audit_ref {input:?} has an empty event_id")]
    EmptyEventId { input: String },
}

impl AuditRef {
    pub fn new(issuer_scope: impl Into<String>, event_id: impl Into<String>) -> Self {
        Self {
            issuer_scope: issuer_scope.into(),
            event_id: event_id.into(),
        }
    }

    /// Parse `<issuer-scope>:<event_id>` per ADR-0011 §6. Splits on the
    /// first `:` only, so an `event_id` containing a colon is preserved
    /// intact while an `issuer_scope` containing one would be mis-parsed —
    /// see the ADR's format rule.
    pub fn parse(input: &str) -> Result<Self, AuditRefParseError> {
        let Some((issuer_scope, event_id)) = input.split_once(':') else {
            return Err(AuditRefParseError::MissingSeparator {
                input: input.to_string(),
            });
        };
        if issuer_scope.is_empty() {
            return Err(AuditRefParseError::EmptyIssuerScope {
                input: input.to_string(),
            });
        }
        if event_id.is_empty() {
            return Err(AuditRefParseError::EmptyEventId {
                input: input.to_string(),
            });
        }
        Ok(AuditRef {
            issuer_scope: issuer_scope.to_string(),
            event_id: event_id.to_string(),
        })
    }
}

impl std::fmt::Display for AuditRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.issuer_scope, self.event_id)
    }
}
