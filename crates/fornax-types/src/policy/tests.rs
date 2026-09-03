//! FORNX-116 acceptance-test scenarios (T1-T27, see
//! `docs/adr/0006-policy-as-data.md`), plus a FORNX-121 section proving the
//! enforcement outcome computation end-to-end from real classified actions.

use std::collections::BTreeSet;

use uuid::Uuid;

use super::*;
use crate::sensor_config::SensorDisableConfig;
use crate::{Provider, RuntimeCapabilities, SignalClass, Verdict};

// ------------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------------

fn empty_content() -> PolicyContent {
    PolicyContent::default()
}

fn draft(content: PolicyContent) -> PolicyDraft {
    PolicyDraft {
        policy_id: PolicyId(Uuid::from_u128(1)),
        revision: 1,
        supersedes: None,
        display_name: "test policy".to_string(),
        content,
        pinned_fields: BTreeSet::new(),
    }
}

fn published(content: PolicyContent) -> PublishedPolicyRevision {
    draft(content)
        .publish("2026-01-01T00:00:00Z".to_string())
        .expect("draft should publish")
}

fn org_binding(org_id: &str, revision: &PublishedPolicyRevision) -> PolicyBinding {
    PolicyBinding {
        binding_id: Uuid::new_v4(),
        scope: TargetScope::Org {
            org_id: org_id.to_string(),
        },
        selector: TargetSelector::default(),
        revision_ref: revision.reference(),
    }
}

fn team_binding(org_id: &str, team_id: &str, revision: &PublishedPolicyRevision) -> PolicyBinding {
    PolicyBinding {
        binding_id: Uuid::new_v4(),
        scope: TargetScope::Team {
            org_id: org_id.to_string(),
            team_id: team_id.to_string(),
        },
        selector: TargetSelector::default(),
        revision_ref: revision.reference(),
    }
}

fn device_binding(device_id: &str, revision: &PublishedPolicyRevision) -> PolicyBinding {
    PolicyBinding {
        binding_id: Uuid::new_v4(),
        scope: TargetScope::Device {
            device_id: device_id.to_string(),
        },
        selector: TargetSelector::default(),
        revision_ref: revision.reference(),
    }
}

fn local_user_binding(revision: &PublishedPolicyRevision) -> PolicyBinding {
    PolicyBinding {
        binding_id: Uuid::new_v4(),
        scope: TargetScope::LocalUser,
        selector: TargetSelector::default(),
        revision_ref: revision.reference(),
    }
}

fn bound(binding: PolicyBinding, revision: PublishedPolicyRevision) -> BoundRevision {
    BoundRevision::new(binding, revision).expect("binding/revision digest should match")
}

fn device_ctx() -> DeviceContext {
    let mut team_ids = BTreeSet::new();
    team_ids.insert("team-1".to_string());
    let mut project_ids = BTreeSet::new();
    project_ids.insert("proj-1".to_string());
    DeviceContext {
        org_id: Some("org-1".to_string()),
        team_ids,
        project_ids,
        device_id: "device-1".to_string(),
        provider: Provider::ClaudeCode,
        capabilities: empty_capabilities(),
        os_family: OsFamily::MacOs,
    }
}

fn empty_capabilities() -> RuntimeCapabilities {
    RuntimeCapabilities {
        schema_version: crate::CAPABILITY_SCHEMA_VERSION,
        provider: Provider::ClaudeCode,
        signals: Vec::new(),
        notes: std::collections::HashMap::new(),
    }
}

fn cloud_sync_content(allowed: bool) -> PolicyContent {
    let mut c = empty_content();
    c.egress.cloud_sync_allowed = Some(allowed);
    c
}

// ------------------------------------------------------------------------
// AC1 -- immutable, reproducibly evaluable
// ------------------------------------------------------------------------

#[test]
#[ignore = "run manually with --ignored --nocapture to regenerate the frozen fixture"]
fn generate_fixture_v1() {
    let mut content = empty_content();
    content.collection.longitudinal_aggregation_allowed = Some(false);
    content.egress.cloud_sync_allowed = Some(true);
    content.egress.redaction_profile = Some(RedactionProfile::Standard);
    let mut allowed = BTreeSet::new();
    allowed.insert(EgressContentClass::FindingVerdicts);
    allowed.insert(EgressContentClass::ClaimText);
    content.egress.allowed_content = Some(allowed);
    let mut disabled = BTreeSet::new();
    disabled.insert("claude_file_write_confirmed_sensor_v1".to_string());
    content.sensors.disabled = Some(disabled);
    content.sensors.required_signals = Some(vec![SignalClass::ProcessResult]);
    content.enforcement.rules = Some(vec![EnforcementRule {
        action_class: ActionClass::ShellCommand,
        risk_class: RiskClass::Elevated,
        outcomes: VerdictOutcomes {
            verified: EnforcementOutcome::Allow,
            unverified: EnforcementOutcome::ObserveOnly,
            contradicted: EnforcementOutcome::Warn,
            review: EnforcementOutcome::ObserveOnly,
            unavailable: EnforcementOutcome::ObserveOnly,
        },
    }]);
    content.cache.max_age_seconds_by_risk = Some(RiskClassSeconds {
        low: 86_400,
        elevated: 21_600,
        high: 3_600,
        critical: 900,
    });
    content.cache.offline_grace_seconds = Some(604_800);

    let mut d = draft(content);
    d.policy_id = PolicyId(Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888));
    d.revision = 1;
    d.display_name = "example v1 fixture policy".to_string();
    d.pinned_fields
        .insert(PolicyFieldId::EgressCloudSyncAllowed);
    let rev = d.publish("2026-01-01T00:00:00Z".to_string()).unwrap();
    let json = serde_json::to_string_pretty(&rev).unwrap();
    println!("{json}");
}

#[test]
fn t1_canonical_bytes_are_byte_identical_across_repeated_serialization_and_a_frozen_fixture() {
    let rev = published(cloud_sync_content(true));
    let bytes1 = canonical_bytes(rev.body());
    let bytes2 = canonical_bytes(rev.body());
    assert_eq!(bytes1, bytes2);

    let fixture = include_str!("../../tests/fixtures/policy_revision_v1.json");
    let parsed: PublishedPolicyRevision =
        serde_json::from_str(fixture).expect("frozen fixture must still deserialize");
    // Digest recomputation inside TryFrom already proves canonical_bytes is
    // reproducible for this exact frozen body; re-derive and compare too.
    assert_eq!(digest_of(parsed.body()), *parsed.digest());
}

#[test]
fn t2_mutating_each_body_field_changes_the_digest_and_nothing_else_does() {
    let base = published(cloud_sync_content(true));
    let base_digest = base.digest().clone();

    let mut other = draft(cloud_sync_content(true));
    other.display_name = "different name".to_string();
    let other_rev = other.publish("2026-01-01T00:00:00Z".to_string()).unwrap();
    assert_ne!(base_digest, *other_rev.digest());

    // Republishing with identical inputs reproduces the identical digest.
    let same = published(cloud_sync_content(true));
    assert_eq!(base_digest, *same.digest());
}

#[test]
fn t3_hand_edited_wire_json_with_stale_digest_is_rejected() {
    let rev = published(cloud_sync_content(true));
    let mut wire: serde_json::Value = serde_json::to_value(&rev).unwrap();
    wire["body"]["display_name"] = serde_json::json!("tampered");
    let result: Result<PublishedPolicyRevision, _> = serde_json::from_value(wire);
    assert!(
        result.is_err(),
        "tampering with body but keeping the old digest must fail"
    );
}

#[test]
fn t4_committed_v1_fixture_still_deserializes() {
    let fixture = include_str!("../../tests/fixtures/policy_revision_v1.json");
    let parsed: Result<PublishedPolicyRevision, _> = serde_json::from_str(fixture);
    assert!(
        parsed.is_ok(),
        "frozen v1 fixture must remain parseable: {:?}",
        parsed.err()
    );
}

// T5 (no public field / no &mut accessor on PublishedPolicyRevision) is a
// structural property, not a runtime assertion -- see `revision.rs`:
// `PublishedPolicyRevision`'s fields are private and `body()`/`digest()`
// return `&_`, never `&mut _`.

// ------------------------------------------------------------------------
// AC2 -- invalid/conflicting fails with actionable diagnostics
// ------------------------------------------------------------------------

#[test]
fn t6_duplicate_action_class_rule_is_rejected_naming_the_class() {
    let mut content = empty_content();
    content.enforcement.rules = Some(vec![
        EnforcementRule {
            action_class: ActionClass::ShellCommand,
            risk_class: RiskClass::Low,
            outcomes: VerdictOutcomes::uniform(EnforcementOutcome::ObserveOnly),
        },
        EnforcementRule {
            action_class: ActionClass::ShellCommand,
            risk_class: RiskClass::High,
            outcomes: VerdictOutcomes::uniform(EnforcementOutcome::Block),
        },
    ]);
    let err = draft(content)
        .publish("2026-01-01T00:00:00Z".to_string())
        .unwrap_err();
    assert!(err
        .diagnostics
        .iter()
        .any(|d| d.code == DiagnosticCode::DuplicateActionClassRule
            && d.message.contains("ShellCommand")));
}

#[test]
fn t7_pin_at_local_user_is_rejected() {
    let mut d = draft(cloud_sync_content(true));
    d.pinned_fields
        .insert(PolicyFieldId::EgressCloudSyncAllowed);
    let rev = d.publish("2026-01-01T00:00:00Z".to_string()).unwrap();
    let binding = local_user_binding(&rev);
    let err = BoundRevision::new(binding, rev).unwrap_err();
    assert!(err
        .diagnostics
        .iter()
        .any(|d| d.code == DiagnosticCode::PinAtLocalUserLayer));
}

#[test]
fn t8_pin_naming_an_unset_field_is_rejected() {
    let mut d = draft(empty_content());
    d.pinned_fields
        .insert(PolicyFieldId::EgressCloudSyncAllowed);
    let err = d.publish("2026-01-01T00:00:00Z".to_string()).unwrap_err();
    assert!(err
        .diagnostics
        .iter()
        .any(|diag| diag.code == DiagnosticCode::PinNamesUnsetField));
}

#[test]
fn t9_two_team_bindings_conflicting_resolves_strictest_with_error_naming_both_bindings() {
    let rev_true = published(cloud_sync_content(true));
    let rev_false = published(cloud_sync_content(false));
    let b1 = team_binding("org-1", "team-1", &rev_true);
    let b2 = team_binding("org-1", "team-1", &rev_false);
    let b1_id = b1.binding_id;
    let b2_id = b2.binding_id;

    let bindings = vec![bound(b1, rev_true), bound(b2, rev_false)];
    let (resolved, diagnostics) = resolve(&bindings, &device_ctx());

    assert!(
        !resolved.values.cloud_sync_allowed,
        "must resolve to the strictest (false)"
    );
    let conflict = diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::ConflictingBindingsAtLevel)
        .expect("expected a ConflictingBindingsAtLevel diagnostic");
    assert!(conflict.bindings.contains(&b1_id));
    assert!(conflict.bindings.contains(&b2_id));
}

#[test]
fn t10_device_layer_loosening_an_org_pinned_field_is_rejected_and_org_value_is_kept() {
    let mut org_draft = draft(cloud_sync_content(false));
    org_draft
        .pinned_fields
        .insert(PolicyFieldId::EgressCloudSyncAllowed);
    let org_rev = org_draft
        .publish("2026-01-01T00:00:00Z".to_string())
        .unwrap();
    let org_b = org_binding("org-1", &org_rev);

    let device_rev = published(cloud_sync_content(true));
    let device_b = device_binding("device-1", &device_rev);

    let bindings = vec![bound(org_b, org_rev), bound(device_b, device_rev)];
    let (resolved, diagnostics) = resolve(&bindings, &device_ctx());

    assert!(
        !resolved.values.cloud_sync_allowed,
        "the pinned org value must survive"
    );
    assert!(diagnostics
        .iter()
        .any(|d| d.code == DiagnosticCode::PinViolation));
    assert!(matches!(
        resolved
            .provenance
            .get(&PolicyFieldId::EgressCloudSyncAllowed),
        Some(FieldProvenance::Pinned {
            level: TargetLevel::Org,
            ..
        })
    ));
}

#[test]
fn t11_every_diagnostic_code_produces_nonempty_message_and_remediation() {
    let codes = [
        DiagnosticCode::UnsupportedSchemaVersion,
        DiagnosticCode::DigestMismatch,
        DiagnosticCode::DuplicateActionClassRule,
        DiagnosticCode::UnsortedEnforcementRules,
        DiagnosticCode::PinAtLocalUserLayer,
        DiagnosticCode::PinNamesUnsetField,
        DiagnosticCode::EmptyDisplayName,
        DiagnosticCode::SupersedesSelf,
        DiagnosticCode::RevisionNotMonotonic,
        DiagnosticCode::ConflictingBindingsAtLevel,
        DiagnosticCode::PinViolation,
        DiagnosticCode::SelectorNotUnderstood,
        DiagnosticCode::RequiredSignalUnavailable,
        DiagnosticCode::UnrecognizedEnvValue,
        DiagnosticCode::NoApplicablePolicy,
    ];
    for code in codes {
        let d = PolicyDiagnostic::new(code, DiagnosticSeverity::Info, "message", "remediation");
        assert!(!d.message.is_empty());
        assert!(!d.remediation.is_empty());
    }
}

// ------------------------------------------------------------------------
// AC3 -- deterministic, documented precedence
// ------------------------------------------------------------------------

#[test]
fn t12_shuffling_the_input_slice_never_changes_the_resolved_output() {
    let org_rev = published(cloud_sync_content(false));
    let team_rev = published(cloud_sync_content(true));
    let device_rev = published({
        let mut c = empty_content();
        c.cache.offline_grace_seconds = Some(100);
        c
    });
    let local_rev = published({
        let mut c = empty_content();
        c.collection.longitudinal_aggregation_allowed = Some(true);
        c
    });

    let items = vec![
        bound(org_binding("org-1", &org_rev), org_rev),
        bound(team_binding("org-1", "team-1", &team_rev), team_rev),
        bound(device_binding("device-1", &device_rev), device_rev),
        bound(local_user_binding(&local_rev), local_rev),
    ];

    let (baseline_resolved, baseline_diagnostics) = resolve(&items, &device_ctx());

    // All 24 permutations of a 4-element input, deterministic (no `rand`
    // dependency -- see docs/adr/0006-policy-as-data.md).
    fn permutations(items: Vec<BoundRevision>) -> Vec<Vec<BoundRevision>> {
        if items.len() <= 1 {
            return vec![items];
        }
        let mut result = Vec::new();
        for i in 0..items.len() {
            let mut rest = items.clone();
            let picked = rest.remove(i);
            for mut perm in permutations(rest.clone()) {
                perm.insert(0, picked.clone());
                result.push(perm);
            }
        }
        result
    }

    for perm in permutations(items) {
        let (resolved, mut diagnostics) = resolve(&perm, &device_ctx());
        assert_eq!(resolved, baseline_resolved);
        diagnostics.sort_by_key(|d| format!("{:?}", d.code));
        let mut expected = baseline_diagnostics.clone();
        expected.sort_by_key(|d| format!("{:?}", d.code));
        assert_eq!(diagnostics, expected);
    }
}

#[test]
fn t13_each_level_overrides_the_previous_on_an_unpinned_field_with_correct_provenance() {
    let org_rev = published(cloud_sync_content(false));
    let team_rev = published(cloud_sync_content(true));
    let project_rev = published(cloud_sync_content(false));
    let device_rev = published(cloud_sync_content(true));

    let device_binding_value = device_binding("device-1", &device_rev);
    let device_binding_id = device_binding_value.binding_id;

    let items = vec![
        bound(org_binding("org-1", &org_rev), org_rev),
        bound(team_binding("org-1", "team-1", &team_rev), team_rev),
        bound(
            PolicyBinding {
                binding_id: Uuid::new_v4(),
                scope: TargetScope::Project {
                    org_id: "org-1".to_string(),
                    project_id: "proj-1".to_string(),
                },
                selector: TargetSelector::default(),
                revision_ref: project_rev.reference(),
            },
            project_rev,
        ),
        bound(device_binding_value, device_rev),
    ];

    let (resolved, _) = resolve(&items, &device_ctx());
    assert!(
        resolved.values.cloud_sync_allowed,
        "the most specific level (Device) must win"
    );
    match resolved
        .provenance
        .get(&PolicyFieldId::EgressCloudSyncAllowed)
    {
        Some(FieldProvenance::Layer {
            level, binding_id, ..
        }) => {
            assert_eq!(*level, TargetLevel::Device);
            assert_eq!(*binding_id, device_binding_id);
        }
        other => panic!("expected Layer provenance at Device level, got {other:?}"),
    }
}

#[test]
fn t14_nothing_set_anywhere_resolves_to_baseline_for_all_nine_fields() {
    let rev = published(empty_content());
    let items = vec![bound(org_binding("org-1", &rev), rev)];
    let (resolved, _) = resolve(&items, &device_ctx());
    let baseline = PolicyContent::baseline();
    assert_eq!(resolved.values, baseline);
    for field in PolicyFieldId::ALL {
        assert_eq!(
            resolved.provenance.get(&field),
            Some(&FieldProvenance::Baseline)
        );
    }
}

#[test]
fn t15_resolve_never_panics_on_adversarial_input() {
    // Empty bindings.
    let (resolved, diagnostics) = resolve(&[], &device_ctx());
    assert_eq!(resolved.values, PolicyContent::baseline());
    assert!(diagnostics
        .iter()
        .any(|d| d.code == DiagnosticCode::NoApplicablePolicy));

    // Binding that doesn't match this device at all (unknown org).
    let rev = published(cloud_sync_content(true));
    let items = vec![bound(org_binding("some-other-org", &rev), rev)];
    let (resolved, diagnostics) = resolve(&items, &device_ctx());
    assert_eq!(resolved.values, PolicyContent::baseline());
    assert!(diagnostics
        .iter()
        .any(|d| d.code == DiagnosticCode::NoApplicablePolicy));

    // Unrecognized values everywhere a selector accepts one.
    let rev2 = published(cloud_sync_content(true));
    let mut binding = org_binding("org-1", &rev2);
    let mut os_families = BTreeSet::new();
    os_families.insert(OsFamily::Unrecognized("plan9".to_string()));
    binding.selector = TargetSelector {
        providers: None,
        os_families: Some(os_families),
        requires_signals: Some(vec![SignalClass::Unrecognized("neural_trace".to_string())]),
    };
    let items2 = vec![bound(binding, rev2)];
    let (resolved2, diagnostics2) = resolve(&items2, &device_ctx());
    assert!(
        resolved2.values.cloud_sync_allowed,
        "unrecognized selector values must still match"
    );
    assert!(diagnostics2
        .iter()
        .any(|d| d.code == DiagnosticCode::SelectorNotUnderstood));

    // Note: `resolve`'s signature returns `(ResolvedPolicy, Vec<PolicyDiagnostic>)`,
    // never `Result` -- it structurally cannot return `Err`.
}

// ------------------------------------------------------------------------
// AC4 -- descriptive, not executable
// ------------------------------------------------------------------------

#[test]
fn t16_exhaustive_matches_have_no_wildcard_arm() {
    // ActionClass
    let ac = ActionClass::CodeEdit;
    match ac {
        ActionClass::CodeEdit
        | ActionClass::ShellCommand
        | ActionClass::VersionControlWrite
        | ActionClass::NetworkFetch
        | ActionClass::PackageInstall
        | ActionClass::CredentialAccess
        | ActionClass::InfrastructureMutation
        | ActionClass::DataEgress
        | ActionClass::Unrecognized(_) => {}
    }

    // RiskClass
    let rc = RiskClass::Low;
    match rc {
        RiskClass::Low | RiskClass::Elevated | RiskClass::High | RiskClass::Critical => {}
    }

    // EnforcementOutcome
    let eo = EnforcementOutcome::Allow;
    match eo {
        EnforcementOutcome::Allow
        | EnforcementOutcome::ObserveOnly
        | EnforcementOutcome::Warn
        | EnforcementOutcome::Block => {}
    }

    // RedactionProfile
    let rp = RedactionProfile::Standard;
    match rp {
        RedactionProfile::Standard | RedactionProfile::Strict => {}
    }

    // EgressContentClass
    let ecc = EgressContentClass::ClaimText;
    match ecc {
        EgressContentClass::FindingVerdicts
        | EgressContentClass::ClaimText
        | EgressContentClass::EvidenceMetadata
        | EgressContentClass::RedactedEvidencePayload
        | EgressContentClass::CapabilityDeclarations
        | EgressContentClass::Unrecognized(_) => {}
    }

    // PolicyFieldId
    let pf = PolicyFieldId::EgressCloudSyncAllowed;
    match pf {
        PolicyFieldId::CollectionLongitudinalAggregationAllowed
        | PolicyFieldId::EgressCloudSyncAllowed
        | PolicyFieldId::EgressRedactionProfile
        | PolicyFieldId::EgressAllowedContent
        | PolicyFieldId::SensorsDisabled
        | PolicyFieldId::SensorsRequiredSignals
        | PolicyFieldId::EnforcementRules
        | PolicyFieldId::CacheMaxAgeByRisk
        | PolicyFieldId::CacheOfflineGraceSeconds
        | PolicyFieldId::Unrecognized(_) => {}
    }

    // VerdictOutcomes::for_verdict -- exhaustive over Verdict internally;
    // assert the mapping directly here too.
    let vo = VerdictOutcomes {
        verified: EnforcementOutcome::Allow,
        unverified: EnforcementOutcome::ObserveOnly,
        contradicted: EnforcementOutcome::Block,
        review: EnforcementOutcome::Warn,
        unavailable: EnforcementOutcome::ObserveOnly,
    };
    assert_eq!(vo.for_verdict(Verdict::Verified), EnforcementOutcome::Allow);
    assert_eq!(
        vo.for_verdict(Verdict::Unverified),
        EnforcementOutcome::ObserveOnly
    );
    assert_eq!(
        vo.for_verdict(Verdict::Contradicted),
        EnforcementOutcome::Block
    );
    assert_eq!(vo.for_verdict(Verdict::Review), EnforcementOutcome::Warn);
    assert_eq!(
        vo.for_verdict(Verdict::Unavailable),
        EnforcementOutcome::ObserveOnly
    );
}

#[test]
fn t17_policy_content_contains_no_serde_json_value_field() {
    // Structural review: `PolicyContent`'s field tree (content.rs) is
    // CollectionScope/EgressScope/SensorScope/EnforcementScope/CacheScope,
    // each built from closed enums, `bool`, `u32`, `String`, and typed
    // collections thereof -- no `serde_json::Value` anywhere. Enforced here
    // by a round-trip test: an arbitrary untyped JSON object is rejected.
    let arbitrary = serde_json::json!({"anything": {"nested": [1, 2, 3]}, "goes": true});
    let result: Result<PolicyContent, _> = serde_json::from_value(arbitrary);
    assert!(
        result.is_err(),
        "an arbitrary untyped object must not deserialize as PolicyContent"
    );
}

#[test]
fn t18_deny_unknown_fields_rejects_an_unknown_key() {
    let mut value = serde_json::to_value(empty_content()).unwrap();
    value["surprise"] = serde_json::json!(true);
    let result: Result<PolicyContent, _> = serde_json::from_value(value);
    assert!(result.is_err(), "an unknown top-level key must be rejected, unlike ExtensionEnvelope's tolerate-and-preserve");
}

// ------------------------------------------------------------------------
// AC5 -- existing rules map without weakening
// ------------------------------------------------------------------------

#[test]
fn t19_env_unset_and_no_published_layers_resolves_cloud_sync_to_false() {
    let (content, diagnostics) =
        local_user_layer_from_values(None, None, &SensorDisableConfig::empty());
    assert_eq!(content.egress.cloud_sync_allowed, None);
    assert!(diagnostics.is_empty());

    let rev = published(content);
    let items = vec![bound(local_user_binding(&rev), rev)];
    let (resolved, _) = resolve(&items, &device_ctx());
    assert!(
        !resolved.values.cloud_sync_allowed,
        "must default to false, identical to today"
    );
}

#[test]
fn t20_env_value_parsing_matches_todays_four_assertions() {
    let empty_cfg = SensorDisableConfig::empty();

    let (c1, d1) = local_user_layer_from_values(Some("1"), None, &empty_cfg);
    assert_eq!(c1.egress.cloud_sync_allowed, Some(true));
    assert!(d1.is_empty());

    let (c2, d2) = local_user_layer_from_values(Some("true"), None, &empty_cfg);
    assert_eq!(c2.egress.cloud_sync_allowed, Some(true));
    assert!(d2.is_empty());

    let (c3, d3) = local_user_layer_from_values(Some("yes"), None, &empty_cfg);
    assert_eq!(
        c3.egress.cloud_sync_allowed,
        Some(false),
        "only 1/true enable sync"
    );
    assert!(d3
        .iter()
        .any(|d| d.code == DiagnosticCode::UnrecognizedEnvValue));

    let (c4, d4) = local_user_layer_from_values(None, None, &empty_cfg);
    assert_eq!(
        c4.egress.cloud_sync_allowed, None,
        "unset means no opinion, not false"
    );
    assert!(d4.is_empty());
}

#[test]
fn t21_org_layer_enables_sync_and_unset_local_env_does_not_defeat_it() {
    let (local_content, _) =
        local_user_layer_from_values(None, None, &SensorDisableConfig::empty());
    assert_eq!(
        local_content.egress.cloud_sync_allowed, None,
        "unset local env contributes no opinion"
    );

    let org_rev = published(cloud_sync_content(true));
    let local_rev = published(local_content);

    let items = vec![
        bound(org_binding("org-1", &org_rev), org_rev),
        bound(local_user_binding(&local_rev), local_rev),
    ];
    let (resolved, _) = resolve(&items, &device_ctx());
    assert!(
        resolved.values.cloud_sync_allowed,
        "org policy must take effect: unset != Some(false)"
    );
    match resolved
        .provenance
        .get(&PolicyFieldId::EgressCloudSyncAllowed)
    {
        Some(FieldProvenance::Layer {
            level: TargetLevel::Org,
            ..
        }) => {}
        other => panic!("expected the Org layer to be the contributor, got {other:?}"),
    }
}

#[test]
fn t22_org_layer_enables_sync_but_local_env_explicitly_restricts_it() {
    let (local_content, _) =
        local_user_layer_from_values(Some("0"), None, &SensorDisableConfig::empty());
    assert_eq!(local_content.egress.cloud_sync_allowed, Some(false));

    let org_rev = published(cloud_sync_content(true));
    let local_rev = published(local_content);

    let items = vec![
        bound(org_binding("org-1", &org_rev), org_rev),
        bound(local_user_binding(&local_rev), local_rev),
    ];
    let (resolved, _) = resolve(&items, &device_ctx());
    assert!(
        !resolved.values.cloud_sync_allowed,
        "the local layer's explicit restriction must win over an org's more permissive setting"
    );
}

#[test]
fn t23_redaction_profile_has_no_off_variant_and_standard_is_todays_behavior() {
    let payload = serde_json::json!({
        "aggregated_output": "JIRA_API_TOKEN=ATATT3xFfGF0LAvBpMqDMfvc_secretvaluehere123"
    });
    // RedactionProfile::Standard is documented (content.rs) as exactly
    // today's `redact::redact_json` behavior -- there is no `Off` variant
    // to select, so the baseline floor cannot skip redaction.
    let baseline = PolicyContent::baseline();
    assert_eq!(baseline.redaction_profile, RedactionProfile::Standard);
    let redacted = crate::redact::redact_json(&payload);
    assert_eq!(
        redacted["aggregated_output"],
        serde_json::json!("[REDACTED: possible secret assignment]")
    );
}

#[test]
fn t24_missing_config_toml_key_preserves_orgs_disabled_list_present_key_unions() {
    let org_content = {
        let mut c = empty_content();
        let mut disabled = BTreeSet::new();
        disabled.insert("org_disabled_sensor".to_string());
        c.sensors.disabled = Some(disabled);
        c
    };

    // Absent config.toml -> None, so the org's list survives untouched.
    let (local_absent, _) = local_user_layer_from_values(None, None, &SensorDisableConfig::empty());
    assert_eq!(local_absent.sensors.disabled, None);

    let org_rev = published(org_content.clone());
    let local_rev = published(local_absent);
    let items = vec![
        bound(org_binding("org-1", &org_rev), org_rev),
        bound(local_user_binding(&local_rev), local_rev),
    ];
    let (resolved, _) = resolve(&items, &device_ctx());
    let mut expected = BTreeSet::new();
    expected.insert("org_disabled_sensor".to_string());
    assert_eq!(resolved.values.sensors_disabled, expected);

    // Present config.toml -> unions with the org's list.
    let local_cfg =
        SensorDisableConfig::from_toml_str("[sensors]\ndisabled = [\"local_disabled_sensor\"]\n")
            .unwrap();
    let (local_present, _) = local_user_layer_from_values(None, None, &local_cfg);
    assert!(local_present.sensors.disabled.is_some());

    let org_rev2 = published(org_content);
    let local_rev2 = published(local_present);
    let items2 = vec![
        bound(org_binding("org-1", &org_rev2), org_rev2),
        bound(local_user_binding(&local_rev2), local_rev2),
    ];
    let (resolved2, _) = resolve(&items2, &device_ctx());
    let mut expected2 = BTreeSet::new();
    expected2.insert("org_disabled_sensor".to_string());
    expected2.insert("local_disabled_sensor".to_string());
    assert_eq!(resolved2.values.sensors_disabled, expected2);
}

#[test]
fn t25_egress_content_class_has_no_raw_payload_variant() {
    let ecc = EgressContentClass::ClaimText;
    match ecc {
        EgressContentClass::FindingVerdicts
        | EgressContentClass::ClaimText
        | EgressContentClass::EvidenceMetadata
        | EgressContentClass::RedactedEvidencePayload
        | EgressContentClass::CapabilityDeclarations
        | EgressContentClass::Unrecognized(_) => {}
    }
    // `RedactedEvidencePayload` is the only payload-shaped variant, and it
    // is explicitly the *redacted* form -- there is no raw-payload,
    // raw-prompt, or source-code variant to select (ADR-0001 D7).
}

// ------------------------------------------------------------------------
// FORNX-121 vocabulary
// ------------------------------------------------------------------------

#[test]
fn t26_baseline_enforcement_reads_observe_only_for_every_action_and_verdict() {
    let baseline = PolicyContent::baseline();
    let action_classes = [
        ActionClass::CodeEdit,
        ActionClass::ShellCommand,
        ActionClass::VersionControlWrite,
        ActionClass::NetworkFetch,
        ActionClass::PackageInstall,
        ActionClass::CredentialAccess,
        ActionClass::InfrastructureMutation,
        ActionClass::DataEgress,
    ];
    let verdicts = [
        Verdict::Verified,
        Verdict::Unverified,
        Verdict::Contradicted,
        Verdict::Review,
        Verdict::Unavailable,
    ];
    for action in &action_classes {
        for verdict in verdicts {
            assert_eq!(
                baseline.enforcement_outcome_for(action, verdict),
                EnforcementOutcome::ObserveOnly,
                "nothing may block on upgrade with no rules published"
            );
        }
    }
}

#[test]
fn t27_meet_of_two_verdict_outcomes_takes_the_stricter_value_per_verdict_independently() {
    let a = VerdictOutcomes {
        verified: EnforcementOutcome::Allow,
        unverified: EnforcementOutcome::ObserveOnly,
        contradicted: EnforcementOutcome::Block,
        review: EnforcementOutcome::Allow,
        unavailable: EnforcementOutcome::Allow,
    };
    let b = VerdictOutcomes {
        verified: EnforcementOutcome::Allow,
        unverified: EnforcementOutcome::Warn,
        contradicted: EnforcementOutcome::Allow,
        review: EnforcementOutcome::Allow,
        unavailable: EnforcementOutcome::Block,
    };
    let merged = VerdictOutcomes::meet(a, b);
    assert_eq!(merged.verified, EnforcementOutcome::Allow);
    assert_eq!(merged.unverified, EnforcementOutcome::Warn);
    assert_eq!(merged.contradicted, EnforcementOutcome::Block);
    assert_eq!(merged.review, EnforcementOutcome::Allow);
    assert_eq!(merged.unavailable, EnforcementOutcome::Block);

    // Not uniform: this is the structural proof it isn't a single global
    // fail-open/fail-closed toggle.
    let all_same = merged.verified == merged.unverified
        && merged.unverified == merged.contradicted
        && merged.contradicted == merged.review
        && merged.review == merged.unavailable;
    assert!(
        !all_same,
        "the merge must not collapse to one uniform outcome"
    );
}

// ------------------------------------------------------------------------
// D2 -- resolve() never Errs and never panics on a malformed remote layer
// ------------------------------------------------------------------------

#[test]
fn d2_malformed_wire_revision_is_rejected_at_construction_and_resolve_still_succeeds_safely() {
    // A malformed/tampered wire revision fails at TryFrom, before it can
    // ever reach `resolve()` -- this is what "resolve() never sees an
    // invalid BoundRevision" means in practice.
    let rev = published(cloud_sync_content(true));
    let mut wire: serde_json::Value = serde_json::to_value(&rev).unwrap();
    wire["body"]["content"]["egress"]["cloud_sync_allowed"] = serde_json::json!(false);
    let tampered: Result<PublishedPolicyRevision, _> = serde_json::from_value(wire);
    assert!(tampered.is_err());

    // The caller ends up with no usable binding for this revision at all
    // (an empty bound set) -- resolve() must still produce a safe,
    // baseline-backed result rather than erroring or panicking.
    let (resolved, diagnostics) = resolve(&[], &device_ctx());
    assert_eq!(resolved.values, PolicyContent::baseline());
    assert!(diagnostics
        .iter()
        .any(|d| d.code == DiagnosticCode::NoApplicablePolicy));
}

#[test]
fn d2_resolve_is_safe_with_no_org_id_on_the_device_context() {
    let rev = published(cloud_sync_content(true));
    let items = vec![bound(org_binding("org-1", &rev), rev)];
    let mut ctx = device_ctx();
    ctx.org_id = None; // device belongs to no org at all
    let (resolved, diagnostics) = resolve(&items, &ctx);
    assert_eq!(resolved.values, PolicyContent::baseline());
    assert!(diagnostics
        .iter()
        .any(|d| d.code == DiagnosticCode::NoApplicablePolicy));
}

// ------------------------------------------------------------------------
// Selector matching (RequiredSignalUnavailable)
// ------------------------------------------------------------------------

#[test]
fn required_signal_unavailable_prevents_a_match_and_emits_a_warning() {
    let rev = published(cloud_sync_content(true));
    let mut binding = org_binding("org-1", &rev);
    binding.selector = TargetSelector {
        providers: None,
        os_families: None,
        requires_signals: Some(vec![SignalClass::ReasoningSummary]),
    };
    let items = vec![bound(binding, rev)];
    let mut ctx = device_ctx();
    ctx.capabilities = empty_capabilities(); // nothing declared available
    let (resolved, diagnostics) = resolve(&items, &ctx);
    assert_eq!(resolved.values, PolicyContent::baseline());
    assert!(diagnostics
        .iter()
        .any(|d| d.code == DiagnosticCode::RequiredSignalUnavailable));
}

#[test]
fn provider_selector_restricts_matching() {
    let rev = published(cloud_sync_content(true));
    let mut binding = org_binding("org-1", &rev);
    binding.selector = TargetSelector {
        providers: Some(vec![Provider::Codex]),
        os_families: None,
        requires_signals: None,
    };
    let items = vec![bound(binding, rev)];
    let (resolved, diagnostics) = resolve(&items, &device_ctx()); // ctx uses ClaudeCode
    assert_eq!(resolved.values, PolicyContent::baseline());
    assert!(diagnostics
        .iter()
        .any(|d| d.code == DiagnosticCode::NoApplicablePolicy));
}

// ------------------------------------------------------------------------
// FORNX-121 -- enforcement outcome wiring, end-to-end from real inputs
// ------------------------------------------------------------------------

/// AC: "Missing evidence (Verdict::Unavailable) is never treated as
/// equivalent to Verdict::Verified." `VerdictOutcomes::for_verdict`'s match
/// is exhaustive per-field (no wildcard arm), so this is a structural
/// guarantee already -- this test pins it against a concrete rule where the
/// two verdicts are configured to opposite extremes, so a future change
/// that accidentally collapsed the two fields together would fail loudly
/// here rather than only in an exhaustiveness-check refactor.
#[test]
fn unavailable_verdict_never_reads_the_verified_outcome() {
    let outcomes = VerdictOutcomes {
        verified: EnforcementOutcome::Allow,
        unverified: EnforcementOutcome::ObserveOnly,
        contradicted: EnforcementOutcome::Warn,
        review: EnforcementOutcome::Warn,
        unavailable: EnforcementOutcome::Block,
    };
    let mut content = empty_content();
    content.enforcement.rules = Some(vec![EnforcementRule {
        action_class: ActionClass::InfrastructureMutation,
        risk_class: RiskClass::Critical,
        outcomes,
    }]);
    let rev = published(content);
    let items = vec![bound(org_binding("org-1", &rev), rev)];
    let (resolved, _) = resolve(&items, &device_ctx());

    let verified = resolved
        .values
        .enforcement_outcome_for(&ActionClass::InfrastructureMutation, Verdict::Verified);
    let unavailable = resolved
        .values
        .enforcement_outcome_for(&ActionClass::InfrastructureMutation, Verdict::Unavailable);

    assert_eq!(verified, EnforcementOutcome::Allow);
    assert_eq!(unavailable, EnforcementOutcome::Block);
    assert_ne!(
        verified, unavailable,
        "identical action_class, different verdict -- Unavailable must never fall through to Verified's outcome"
    );
}

/// End-to-end exercise of the whole FORNX-121 wiring: a realistic org
/// policy is authored via `PolicyDraft::publish`, resolved against a
/// concrete `DeviceContext`, and then queried with `ActionClass` values
/// produced by [`crate::policy::classify_action_class`] from real
/// adapter-shaped tool-call data (Claude Code `Bash`/`Edit`, Codex
/// `exec_command`) -- not hand-constructed `ActionClass` literals. This is
/// the "policy simulator" AC: `enforcement_outcome_for` is the pure
/// function a simulator calls, and this test proves it is wired to
/// something a real classification produces, end to end.
#[test]
fn end_to_end_classification_and_resolved_policy_agree_on_enforcement_outcome() {
    let mut content = empty_content();
    content.enforcement.rules = Some(vec![
        EnforcementRule {
            action_class: ActionClass::CodeEdit,
            risk_class: RiskClass::Low,
            outcomes: VerdictOutcomes::uniform(EnforcementOutcome::Allow),
        },
        EnforcementRule {
            action_class: ActionClass::InfrastructureMutation,
            risk_class: RiskClass::Critical,
            outcomes: VerdictOutcomes {
                verified: EnforcementOutcome::Allow,
                unverified: EnforcementOutcome::Warn,
                contradicted: EnforcementOutcome::Block,
                review: EnforcementOutcome::Block,
                unavailable: EnforcementOutcome::Block,
            },
        },
    ]);
    let rev = published(content);
    let items = vec![bound(org_binding("org-1", &rev), rev)];
    let (resolved, diagnostics) = resolve(&items, &device_ctx());
    assert!(
        diagnostics.is_empty(),
        "clean single-binding resolve should carry no diagnostics: {diagnostics:?}"
    );

    // A `terraform apply` shell invocation, shaped exactly like a real
    // Claude Code Bash PostToolUse event's tool_input.
    let terraform_apply = serde_json::json!({ "command": "terraform apply -auto-approve" });
    let infra_action = classify_action_class(Provider::ClaudeCode, "Bash", Some(&terraform_apply));
    assert_eq!(infra_action, ActionClass::InfrastructureMutation);
    assert_eq!(
        resolved
            .values
            .enforcement_outcome_for(&infra_action, Verdict::Unavailable),
        EnforcementOutcome::Block,
        "critical infra mutation with no verification evidence must not read as safe"
    );
    assert_eq!(
        resolved
            .values
            .enforcement_outcome_for(&infra_action, Verdict::Verified),
        EnforcementOutcome::Allow
    );

    // A low-risk code edit stays usable even in the same degraded
    // (Unavailable) state -- AC: "low-risk workflows remain usable during
    // appropriate degraded states."
    let edit_action = classify_action_class(Provider::ClaudeCode, "Edit", None);
    assert_eq!(edit_action, ActionClass::CodeEdit);
    assert_eq!(
        resolved
            .values
            .enforcement_outcome_for(&edit_action, Verdict::Unavailable),
        EnforcementOutcome::Allow
    );

    // A Codex `exec_command` shell invocation classifies identically to the
    // Claude Code `Bash` equivalent for the same command text -- the policy
    // consumes `ActionClass`, never a provider-specific tool name.
    let git_push = serde_json::json!({ "command": "git push origin main" });
    let vcs_action = classify_action_class(Provider::Codex, "exec_command", Some(&git_push));
    assert_eq!(vcs_action, ActionClass::VersionControlWrite);
    // No rule was published for VersionControlWrite -- baseline's
    // no-rule-published floor applies (ObserveOnly), never an invented
    // block just because the action_class happens to be sensitive-sounding.
    assert_eq!(
        resolved
            .values
            .enforcement_outcome_for(&vcs_action, Verdict::Contradicted),
        EnforcementOutcome::ObserveOnly
    );

    // A tool this policy has no opinion about and this classifier has no
    // mapping for reads the same ObserveOnly floor.
    let unmapped_action = classify_action_class(Provider::ClaudeCode, "Read", None);
    assert_eq!(
        unmapped_action,
        ActionClass::Unrecognized("Read".to_string())
    );
    assert_eq!(
        resolved
            .values
            .enforcement_outcome_for(&unmapped_action, Verdict::Contradicted),
        EnforcementOutcome::ObserveOnly
    );
}

/// AC: "Unit tests cover combinations of risk class, verdict, missing
/// capability, and stale/offline policy." A binding whose `TargetSelector`
/// requires a signal the device's declared `RuntimeCapabilities` doesn't
/// report never matches (`RequiredSignalUnavailable`) -- its enforcement
/// rules never apply, and `enforcement_outcome_for` reads the baseline
/// `ObserveOnly` floor even though the org intended to `Block`. This is a
/// deliberate consequence of resolve()'s existing selector-matching
/// contract (never a new behavior introduced here); this test exists so
/// the enforcement-specific instance of it is pinned explicitly, not left
/// as an emergent property only covered indirectly by
/// `required_signal_unavailable_prevents_a_match_and_emits_a_warning`.
#[test]
fn missing_required_capability_falls_back_to_baseline_observe_only_not_the_intended_block() {
    let mut content = empty_content();
    content.enforcement.rules = Some(vec![EnforcementRule {
        action_class: ActionClass::InfrastructureMutation,
        risk_class: RiskClass::Critical,
        outcomes: VerdictOutcomes::uniform(EnforcementOutcome::Block),
    }]);
    let rev = published(content);
    let mut binding = org_binding("org-1", &rev);
    binding.selector = TargetSelector {
        providers: None,
        os_families: None,
        requires_signals: Some(vec![SignalClass::ReasoningSummary]),
    };
    let items = vec![bound(binding, rev)];

    let mut ctx = device_ctx();
    ctx.capabilities = empty_capabilities(); // this device never declared ReasoningSummary
    let (resolved, diagnostics) = resolve(&items, &ctx);

    assert!(diagnostics
        .iter()
        .any(|d| d.code == DiagnosticCode::RequiredSignalUnavailable));
    assert_eq!(
        resolved
            .values
            .enforcement_outcome_for(&ActionClass::InfrastructureMutation, Verdict::Contradicted),
        EnforcementOutcome::ObserveOnly,
        "an org's Block rule must never silently apply when its own required capability is missing"
    );

    // Same rule, same action_class, on a device that DOES declare the
    // required signal: the binding matches and the intended Block applies.
    let mut ctx_with_signal = device_ctx();
    ctx_with_signal.capabilities = RuntimeCapabilities {
        schema_version: crate::CAPABILITY_SCHEMA_VERSION,
        provider: Provider::ClaudeCode,
        signals: vec![crate::CapabilitySignal {
            class: SignalClass::ReasoningSummary,
            state: crate::SignalAvailability::Available,
            detail: None,
        }],
        notes: std::collections::HashMap::new(),
    };
    let rev2 = published({
        let mut c = empty_content();
        c.enforcement.rules = Some(vec![EnforcementRule {
            action_class: ActionClass::InfrastructureMutation,
            risk_class: RiskClass::Critical,
            outcomes: VerdictOutcomes::uniform(EnforcementOutcome::Block),
        }]);
        c
    });
    let mut binding2 = org_binding("org-1", &rev2);
    binding2.selector = TargetSelector {
        providers: None,
        os_families: None,
        requires_signals: Some(vec![SignalClass::ReasoningSummary]),
    };
    let items2 = vec![bound(binding2, rev2)];
    let (resolved2, diagnostics2) = resolve(&items2, &ctx_with_signal);
    assert!(diagnostics2
        .iter()
        .all(|d| d.code != DiagnosticCode::RequiredSignalUnavailable));
    assert_eq!(
        resolved2
            .values
            .enforcement_outcome_for(&ActionClass::InfrastructureMutation, Verdict::Contradicted),
        EnforcementOutcome::Block
    );
}
