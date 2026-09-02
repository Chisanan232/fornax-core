//! Evidence graph: typed claim-to-evidence linkage and explicit
//! missing-evidence markers (FORNX-89, parent epic FORNX-66).
//!
//! Stage 1-3 evidence is flat: a `Claim` exists, `Evidence` rows exist per
//! session, and a `Finding` names which `evidence_ids` a verifier used — but
//! there is no persisted record of *how* a given piece of evidence relates
//! to a claim (supports it? contradicts it?), and a claim with zero linked
//! evidence looks identical to a claim whose expected evidence genuinely
//! could not be collected. This module adds that missing layer, additively:
//!
//! - [`EvidenceLink`]: a typed edge from a `Claim` to a piece of `Evidence`
//!   ([`EvidenceRelation::Supports`]/[`EvidenceRelation::Contradicts`]/
//!   [`EvidenceRelation::Neutral`]). No confidence/weight/fusion here —
//!   combining multiple links into an uncertainty-aware verdict is FORNX-93's
//!   job (epic FORNX-66's "Evidence first, score second" invariant: a score
//!   is a compression of inspectable evidence, computed downstream of it,
//!   never folded back into the evidence layer itself).
//! - [`MissingEvidence`]: a claim's explicit note that evidence of a given
//!   [`crate::SignalClass`] was expected but is
//!   [`crate::SignalAvailability::Unavailable`]/`Unsupported`/
//!   `CollectionFailed`, reusing FORNX-155's existing taxonomy rather than
//!   inventing a parallel one. This is the "missing evidence is explicitly
//!   modeled rather than inferred from empty arrays" AC — "no evidence
//!   found" and "evidence could not exist" must stay distinguishable.
//! - [`EvidenceGraph`]: a read-side aggregate (not itself persisted) bundling
//!   one claim's links and missing-evidence notes for callers like
//!   `fornax-store::Store::evidence_graph_for_claim`.
//!
//! Both `EvidenceLink` and `MissingEvidence` are pure additions alongside
//! `Evidence`/`Claim` — see `docs/adr/0001-architecture-invariants.md`'s D3
//! ("immutable observation before interpretation"): this module never
//! mutates or reinterprets a stored `Evidence`/`Claim` row, it only records
//! new relationship facts about them.
//!
//! # Out of scope (FORNX-89 AC / epic non-goals)
//!
//! - Fusion/uncertainty scoring across links (FORNX-93).
//! - A read-facing API/UI for the graph (FORNX-90, "Evidence Explorer").
//! - New sensor types or signal classes (FORNX-91) — this module only
//!   consumes the existing [`crate::SignalClass`]/[`crate::SignalAvailability`]
//!   taxonomy, it does not extend it.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{SignalAvailability, SignalClass};

/// How one piece of evidence relates to a claim (FORNX-89). Deliberately
/// closed and confidence-free — see module docs for why fusion/weighting is
/// out of scope here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRelation {
    /// This evidence is consistent with the claim being true.
    Supports,
    /// This evidence is inconsistent with the claim being true.
    Contradicts,
    /// This evidence is related to the claim but neither supports nor
    /// contradicts it on its own (e.g. context evidence a verifier consulted
    /// without it moving the verdict either way).
    Neutral,
}

/// A typed edge from a [`crate::Claim`] to a piece of [`crate::Evidence`]
/// (FORNX-89). Additive linkage layer — recording this edge does not change
/// how either the claim or the evidence row is stored or interpreted by
/// existing verifiers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceLink {
    pub id: Uuid,
    pub session_id: String,
    pub claim_id: Uuid,
    pub evidence_id: Uuid,
    pub relation: EvidenceRelation,
    /// RFC3339 timestamp this linkage was recorded (not when the underlying
    /// evidence was observed — see [`crate::Evidence::observed_at`] for
    /// that).
    pub linked_at: String,
}

/// A claim's explicit note that evidence of a given [`SignalClass`] was
/// expected but could not be collected/observed (FORNX-89). Reuses
/// [`SignalClass`]/[`SignalAvailability`] from FORNX-155/157 rather than a
/// parallel taxonomy — see module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissingEvidence {
    pub id: Uuid,
    pub session_id: String,
    pub claim_id: Uuid,
    pub signal_class: SignalClass,
    /// Why it's missing — expected to be one of `Unavailable`/`Unsupported`/
    /// `CollectionFailed`/`Redacted`, though this type does not itself
    /// enforce that subset (it stores whatever `SignalAvailability` the
    /// caller determined, same tolerance the rest of that enum's consumers
    /// already have).
    pub availability: SignalAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// RFC3339 timestamp this absence was noted.
    pub noted_at: String,
}

/// Read-side aggregate for one claim's evidence graph (FORNX-89): everything
/// known to support/contradict/relate to it, plus everything explicitly
/// noted absent. Not itself a persisted row — computed on demand by
/// `fornax-store::Store::evidence_graph_for_claim`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EvidenceGraph {
    pub links: Vec<EvidenceLink>,
    pub missing: Vec<MissingEvidence>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_link(relation: EvidenceRelation) -> EvidenceLink {
        EvidenceLink {
            id: Uuid::new_v4(),
            session_id: "s1".into(),
            claim_id: Uuid::new_v4(),
            evidence_id: Uuid::new_v4(),
            relation,
            linked_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn sample_missing() -> MissingEvidence {
        MissingEvidence {
            id: Uuid::new_v4(),
            session_id: "s1".into(),
            claim_id: Uuid::new_v4(),
            signal_class: SignalClass::ProcessResult,
            availability: SignalAvailability::Unavailable,
            detail: Some("no exit code sensor ran for this claim".into()),
            noted_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    // --- EvidenceRelation: closed three-state vocabulary -----------------

    #[test]
    fn evidence_relation_has_exactly_three_states_with_stable_wire_names() {
        let cases = [
            (EvidenceRelation::Supports, "supports"),
            (EvidenceRelation::Contradicts, "contradicts"),
            (EvidenceRelation::Neutral, "neutral"),
        ];
        for (relation, expected_wire_name) in cases {
            // Exhaustiveness check: every variant must be named here.
            match relation {
                EvidenceRelation::Supports
                | EvidenceRelation::Contradicts
                | EvidenceRelation::Neutral => {}
            }
            let json = serde_json::to_value(relation).unwrap();
            assert_eq!(json, serde_json::json!(expected_wire_name));
            let back: EvidenceRelation = serde_json::from_value(json).unwrap();
            assert_eq!(relation, back);
        }
    }

    // --- EvidenceLink round trip ------------------------------------------

    #[test]
    fn evidence_link_round_trips_through_json() {
        let link = sample_link(EvidenceRelation::Supports);
        let json = serde_json::to_string(&link).unwrap();
        let back: EvidenceLink = serde_json::from_str(&json).unwrap();
        assert_eq!(link, back);
    }

    // --- MissingEvidence round trip --------------------------------------

    #[test]
    fn missing_evidence_round_trips_through_json() {
        let missing = sample_missing();
        let json = serde_json::to_string(&missing).unwrap();
        let back: MissingEvidence = serde_json::from_str(&json).unwrap();
        assert_eq!(missing, back);
    }

    #[test]
    fn missing_evidence_detail_is_optional_and_omitted_when_absent() {
        let mut missing = sample_missing();
        missing.detail = None;
        let json = serde_json::to_value(&missing).unwrap();
        assert!(json.get("detail").is_none());
        let back: MissingEvidence = serde_json::from_value(json).unwrap();
        assert_eq!(back.detail, None);
    }

    // --- EvidenceGraph aggregate -------------------------------------------

    #[test]
    fn evidence_graph_bundles_links_and_missing_independently() {
        let graph = EvidenceGraph {
            links: vec![
                sample_link(EvidenceRelation::Supports),
                sample_link(EvidenceRelation::Supports),
                sample_link(EvidenceRelation::Contradicts),
            ],
            missing: vec![sample_missing()],
        };
        assert_eq!(graph.links.len(), 3);
        assert_eq!(graph.missing.len(), 1);
        assert_eq!(
            graph
                .links
                .iter()
                .filter(|l| l.relation == EvidenceRelation::Supports)
                .count(),
            2
        );
    }

    #[test]
    fn empty_evidence_graph_is_distinguishable_from_a_graph_with_only_missing_notes() {
        // The core product invariant this ticket exists for: zero links plus
        // zero missing notes ("nobody has looked") must remain distinct in
        // principle from zero links plus a non-empty missing list ("we
        // looked, evidence could not be collected"). Both are representable
        // by this type; nothing here collapses one into the other.
        let nobody_looked = EvidenceGraph::default();
        let looked_but_absent = EvidenceGraph {
            links: vec![],
            missing: vec![sample_missing()],
        };
        assert!(nobody_looked.links.is_empty() && nobody_looked.missing.is_empty());
        assert!(looked_but_absent.links.is_empty() && !looked_but_absent.missing.is_empty());
        assert_ne!(nobody_looked, looked_but_absent);
    }
}
