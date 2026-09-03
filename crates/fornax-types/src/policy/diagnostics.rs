//! Policy diagnostics (FORNX-116).
//!
//! Two different jobs, two different signatures — do not merge them.
//! `PolicyDraft::publish`, `BoundRevision::new`, and
//! `TryFrom<PolicyRevisionWire>` return `Result<_, PolicyValidationReport>`:
//! any Error-severity diagnostic means the whole operation failed.
//! `resolve()` returns `(ResolvedPolicy, Vec<PolicyDiagnostic>)` and never
//! `Err` — see `super::resolve`'s module docs (D2).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::resolve::PolicyFieldId;
use super::revision::PolicyRevisionRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
}

/// Stable snake_case wire name for FORNX-117's authoring UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    // -- publish/validate time -> Err -----------------------------------
    UnsupportedSchemaVersion,
    DigestMismatch,
    DuplicateActionClassRule,
    UnsortedEnforcementRules,
    PinAtLocalUserLayer,
    PinNamesUnsetField,
    EmptyDisplayName,
    SupersedesSelf,
    /// Reserved for a future check once a revision *history* exists to
    /// compare against (no `fornax-store` migration in this ticket — see
    /// `docs/adr/0006-policy-as-data.md`). Not emitted by anything in this
    /// crate yet.
    RevisionNotMonotonic,
    // -- resolve time -> diagnostics only, never Err ---------------------
    ConflictingBindingsAtLevel,
    PinViolation,
    SelectorNotUnderstood,
    RequiredSignalUnavailable,
    UnrecognizedEnvValue,
    NoApplicablePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDiagnostic {
    pub code: DiagnosticCode,
    pub severity: DiagnosticSeverity,
    pub field: Option<PolicyFieldId>,
    pub bindings: Vec<Uuid>,
    pub revisions: Vec<PolicyRevisionRef>,
    /// What is wrong. Never empty.
    pub message: String,
    /// What to change. Never empty.
    pub remediation: String,
}

impl PolicyDiagnostic {
    pub fn new(
        code: DiagnosticCode,
        severity: DiagnosticSeverity,
        message: impl Into<String>,
        remediation: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity,
            field: None,
            bindings: Vec::new(),
            revisions: Vec::new(),
            message: message.into(),
            remediation: remediation.into(),
        }
    }

    pub fn with_field(mut self, field: PolicyFieldId) -> Self {
        self.field = Some(field);
        self
    }

    pub fn with_bindings(mut self, bindings: Vec<Uuid>) -> Self {
        self.bindings = bindings;
        self
    }

    pub fn with_revisions(mut self, revisions: Vec<PolicyRevisionRef>) -> Self {
        self.revisions = revisions;
        self
    }
}

/// Returned by `PolicyDraft::publish`, `BoundRevision::new`, and
/// `TryFrom<PolicyRevisionWire>` when at least one Error-severity
/// [`PolicyDiagnostic`] was produced.
#[derive(Debug, Clone, thiserror::Error)]
#[error("policy validation failed with {} diagnostic(s)", diagnostics.len())]
pub struct PolicyValidationReport {
    pub diagnostics: Vec<PolicyDiagnostic>,
}

impl PolicyValidationReport {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == DiagnosticSeverity::Error)
    }
}
