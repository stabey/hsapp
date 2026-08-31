use hs_advisor_api::ScoreFeature;
use hs_planner::{
    DeckCardCount, DeckSignature, GenericFallbackReason, PlaybookCatalog, PlaybookError,
    PlaybookSelectionReason, SideboardCardCount,
};
use hs_types::{BuildId, CardId};

const FULL_WEIGHTS: &str = r#"
  friendly_effective_health: 100
  opponent_effective_health: -100
  friendly_board_attack: 100
  opponent_board_attack: -100
  friendly_board_health: 100
  opponent_board_health: -100
  friendly_board_value: 100
  opponent_board_value: -100
  friendly_hand_size: 100
  opponent_hand_size: -100
  resource_advantage: 100
  mana_efficiency: 100
  playbook_preference: 0
  risk_penalty: -1000
"#;

fn generic_yaml() -> String {
    format!(
        r#"schema_version: 1
id: generic
priority: 0
applies_to:
  build_range: {{ min: 0 }}
  deck_signatures: []
weights:
{FULL_WEIGHTS}risk_weight_milli: 250
win_points: []
explanations: {{}}
"#
    )
}

fn exact_yaml(id: &str, priority: u32, min: u32, max: u32, card_id: &str) -> String {
    format!(
        r#"schema_version: 1
id: {id}
priority: {priority}
applies_to:
  build_range: {{ min: {min}, max: {max} }}
  deck_signatures:
    - main:
        - {{ card_id: {card_id}, count: 1 }}
      sideboard: []
weights:
{FULL_WEIGHTS}risk_weight_milli: 300
win_points:
  - id: pressure
    feature: friendly_board_attack
    comparison: at_least
    threshold: 5
    message: Keep pressure.
explanations:
  friendly_board_attack:
    positive: More pressure.
    negative: Less pressure.
"#
    )
}

fn signature(card_id: &str) -> DeckSignature {
    DeckSignature::new(
        [DeckCardCount {
            card_id: CardId::new(card_id),
            count: 1,
        }],
        [],
    )
    .expect("valid signature")
}

#[test]
fn deck_signature_canonicalizes_order_duplicates_and_sideboard_owner() {
    let signature = DeckSignature::new(
        [
            DeckCardCount {
                card_id: CardId::new("B_002"),
                count: 1,
            },
            DeckCardCount {
                card_id: CardId::new("A_001"),
                count: 1,
            },
            DeckCardCount {
                card_id: CardId::new("B_002"),
                count: 2,
            },
        ],
        [
            SideboardCardCount {
                owner_card_id: CardId::new("OWNER_2"),
                card_id: CardId::new("SIDE_1"),
                count: 1,
            },
            SideboardCardCount {
                owner_card_id: CardId::new("OWNER_1"),
                card_id: CardId::new("SIDE_1"),
                count: 1,
            },
            SideboardCardCount {
                owner_card_id: CardId::new("OWNER_2"),
                card_id: CardId::new("SIDE_1"),
                count: 2,
            },
        ],
    )
    .expect("canonical signature");

    assert_eq!(
        signature
            .main()
            .iter()
            .map(|entry| (entry.card_id.as_str(), entry.count))
            .collect::<Vec<_>>(),
        vec![("A_001", 1), ("B_002", 3)]
    );
    assert_eq!(
        signature
            .sideboard()
            .iter()
            .map(|entry| {
                (
                    entry.owner_card_id.as_str(),
                    entry.card_id.as_str(),
                    entry.count,
                )
            })
            .collect::<Vec<_>>(),
        vec![("OWNER_1", "SIDE_1", 1), ("OWNER_2", "SIDE_1", 3),]
    );

    let different_owner = DeckSignature::new(
        [DeckCardCount {
            card_id: CardId::new("A_001"),
            count: 1,
        }],
        [SideboardCardCount {
            owner_card_id: CardId::new("OWNER_3"),
            card_id: CardId::new("SIDE_1"),
            count: 1,
        }],
    )
    .expect("valid signature");
    assert_ne!(signature, different_owner);
}

#[test]
fn zero_count_and_unstable_card_id_are_rejected() {
    let zero = DeckSignature::new(
        [DeckCardCount {
            card_id: CardId::new("A_001"),
            count: 0,
        }],
        [],
    );
    assert!(zero.is_err());

    let unstable = DeckSignature::new(
        [DeckCardCount {
            card_id: CardId::new("bad id"),
            count: 1,
        }],
        [],
    );
    assert!(unstable.is_err());
}

#[test]
fn exact_selection_and_both_generic_fallback_reasons_are_structured() {
    let catalog = PlaybookCatalog::from_sources([
        ("generic.yaml", generic_yaml()),
        (
            "exact.yaml",
            exact_yaml("standard.example.100", 10, 100, 100, "CARD_1"),
        ),
    ])
    .expect("valid catalog");
    let deck = signature("CARD_1");

    let exact = catalog
        .select(BuildId(100), Some(&deck))
        .expect("exact match");
    assert_eq!(exact.id(), "standard.example.100");
    assert_eq!(exact.weight(ScoreFeature::RiskPenalty), -1000);
    assert_eq!(exact.risk_weight_milli(), 300);
    assert_eq!(exact.win_points()[0].id, "pressure");
    assert!(exact
        .explanations()
        .contains_key(&ScoreFeature::FriendlyBoardAttack));
    assert_eq!(
        exact.selection_reason(),
        &PlaybookSelectionReason::ExactDeckMatch {
            build: BuildId(100)
        }
    );

    let no_signature = catalog
        .select(BuildId(100), None)
        .expect("generic fallback");
    assert_eq!(no_signature.id(), "generic");
    assert_eq!(
        no_signature.selection_reason(),
        &PlaybookSelectionReason::GenericFallback {
            build: BuildId(100),
            reason: GenericFallbackReason::DeckSignatureUnavailable,
        }
    );

    let wrong_build = catalog
        .select(BuildId(101), Some(&deck))
        .expect("cross-build generic fallback");
    assert_eq!(wrong_build.id(), "generic");
    assert_eq!(
        wrong_build.selection_reason(),
        &PlaybookSelectionReason::GenericFallback {
            build: BuildId(101),
            reason: GenericFallbackReason::NoExactDeckMatch,
        }
    );
}

#[test]
fn source_order_does_not_change_selection() {
    let generic = generic_yaml();
    let low = exact_yaml("standard.low.100", 10, 100, 100, "CARD_1");
    let high = exact_yaml("standard.high.100", 20, 100, 100, "CARD_1");
    let forward = PlaybookCatalog::from_sources([
        ("generic.yaml", generic.clone()),
        ("low.yaml", low.clone()),
        ("high.yaml", high.clone()),
    ])
    .expect("valid catalog");
    let reverse = PlaybookCatalog::from_sources([
        ("high.yaml", high),
        ("low.yaml", low),
        ("generic.yaml", generic),
    ])
    .expect("valid catalog");
    let deck = signature("CARD_1");

    assert_eq!(
        forward.select(BuildId(100), Some(&deck)).unwrap().id(),
        reverse.select(BuildId(100), Some(&deck)).unwrap().id()
    );
    assert_eq!(
        forward.select(BuildId(100), Some(&deck)).unwrap().id(),
        "standard.high.100"
    );
}

#[test]
fn equal_priority_overlapping_exact_matches_fail_at_load_time() {
    let error = PlaybookCatalog::from_sources([
        ("generic.yaml", generic_yaml()),
        (
            "left.yaml",
            exact_yaml("standard.left.100", 10, 90, 110, "CARD_1"),
        ),
        (
            "right.yaml",
            exact_yaml("standard.right.100", 10, 100, 120, "CARD_1"),
        ),
    ])
    .expect_err("equal-best exact match must be rejected");

    assert!(matches!(error, PlaybookError::AmbiguousExactMatch { .. }));
}

#[test]
fn duplicate_id_missing_generic_and_invalid_generic_fail_validation() {
    let duplicate = PlaybookCatalog::from_sources([
        ("generic.yaml", generic_yaml()),
        ("duplicate.yaml", generic_yaml()),
    ])
    .expect_err("duplicate IDs must fail");
    assert!(matches!(duplicate, PlaybookError::DuplicateId { .. }));

    let missing = PlaybookCatalog::from_sources([(
        "exact.yaml",
        exact_yaml("standard.example.100", 10, 100, 100, "CARD_1"),
    )])
    .expect_err("generic is mandatory");
    assert!(matches!(missing, PlaybookError::MissingGeneric));

    let invalid_generic = generic_yaml().replace("min: 0", "min: 1");
    let invalid = PlaybookCatalog::from_sources([("generic.yaml", invalid_generic)])
        .expect_err("generic must cover every build");
    assert!(matches!(invalid, PlaybookError::InvalidGeneric));
}

#[test]
fn strict_schema_and_semantic_ranges_fail_before_selection() {
    let unsupported_schema = generic_yaml().replace("schema_version: 1", "schema_version: 2");
    assert!(matches!(
        PlaybookCatalog::from_sources([("generic.yaml", unsupported_schema)]),
        Err(PlaybookError::UnsupportedSchema { .. })
    ));

    let unstable_id = generic_yaml().replace("id: generic", "id: Bad ID");
    assert!(matches!(
        PlaybookCatalog::from_sources([("generic.yaml", unstable_id)]),
        Err(PlaybookError::InvalidId { .. })
    ));

    let unknown_top =
        generic_yaml().replace("schema_version: 1", "schema_version: 1\nsurprise: true");
    assert!(matches!(
        PlaybookCatalog::from_sources([("generic.yaml", unknown_top)]),
        Err(PlaybookError::Yaml { .. })
    ));

    let unknown_nested = generic_yaml().replace(
        "build_range: { min: 0 }",
        "build_range: { min: 0, surprise: true }",
    );
    assert!(matches!(
        PlaybookCatalog::from_sources([("generic.yaml", unknown_nested)]),
        Err(PlaybookError::Yaml { .. })
    ));

    let missing_weight = generic_yaml().replace("  mana_efficiency: 100\n", "");
    assert!(matches!(
        PlaybookCatalog::from_sources([("generic.yaml", missing_weight)]),
        Err(PlaybookError::MissingWeight {
            feature: ScoreFeature::ManaEfficiency,
            ..
        })
    ));

    let unknown_feature = generic_yaml().replace(
        "  mana_efficiency: 100",
        "  mana_efficiency: 100\n  invented_progress: 1",
    );
    assert!(matches!(
        PlaybookCatalog::from_sources([("generic.yaml", unknown_feature)]),
        Err(PlaybookError::Yaml { .. })
    ));

    let bad_weight = generic_yaml().replace(
        "  friendly_effective_health: 100",
        "  friendly_effective_health: 1000001",
    );
    assert!(matches!(
        PlaybookCatalog::from_sources([("generic.yaml", bad_weight)]),
        Err(PlaybookError::InvalidWeight { .. })
    ));

    let bad_risk = generic_yaml().replace("risk_weight_milli: 250", "risk_weight_milli: -1");
    assert!(matches!(
        PlaybookCatalog::from_sources([("generic.yaml", bad_risk)]),
        Err(PlaybookError::InvalidRiskWeight { .. })
    ));

    let bad_build = exact_yaml("standard.example.100", 10, 101, 100, "CARD_1");
    assert!(matches!(
        PlaybookCatalog::from_sources([
            ("generic.yaml", generic_yaml()),
            ("exact.yaml", bad_build)
        ]),
        Err(PlaybookError::InvalidBuildRange { .. })
    ));
}
