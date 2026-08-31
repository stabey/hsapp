use hs_advisor_api::ScoreFeature;
use hs_planner::{
    DeckCardCount, DeckSignature, GenericFallbackReason, PlaybookCatalog, PlaybookSelectionReason,
    SideboardCardCount,
};
use hs_types::{BuildId, CardId};

const GENERIC: &str = include_str!("../../../rules/playbooks/generic.yaml");
const QUEST_HUNTER: &str =
    include_str!("../../../rules/playbooks/standard.quest_hunter.249896.yaml");

fn card(card_id: &str, count: u16) -> DeckCardCount {
    DeckCardCount {
        card_id: CardId::new(card_id),
        count,
    }
}

#[test]
fn checked_in_playbooks_parse_and_select_the_exact_249896_deck() {
    let catalog = PlaybookCatalog::from_sources([
        ("generic.yaml", GENERIC),
        ("standard.quest_hunter.249896.yaml", QUEST_HUNTER),
    ])
    .expect("repository Playbooks must validate");
    assert_eq!(catalog.playbooks().len(), 2);

    let signature = DeckSignature::new(
        [
            card("CATA_554", 1),
            card("CORE_BAR_801", 2),
            card("CORE_DS1_184", 2),
            card("CORE_EDR_004_2026", 2),
            card("CORE_EX1_014", 1),
            card("CORE_SW_072", 1),
            card("CORE_UNG_912", 2),
            card("EDR_453", 1),
            card("EDR_846", 1),
            card("JAIL_831", 1),
            card("JAIL_877", 2),
            card("JAIL_878", 2),
            card("JAIL_EVENT_100", 1),
            card("TIME_053", 2),
            card("TLC_366", 2),
            card("TLC_603", 2),
            card("TLC_823", 2),
            card("TLC_825", 2),
            card("TLC_830", 1),
        ],
        [
            SideboardCardCount {
                owner_card_id: CardId::new("JAIL_831"),
                card_id: CardId::new("EDR_527"),
                count: 1,
            },
            SideboardCardCount {
                owner_card_id: CardId::new("JAIL_831"),
                card_id: CardId::new("EDR_489"),
                count: 1,
            },
            SideboardCardCount {
                owner_card_id: CardId::new("JAIL_831"),
                card_id: CardId::new("TLC_436"),
                count: 1,
            },
        ],
    )
    .expect("authoritative normalized signature");
    assert_eq!(
        signature
            .main()
            .iter()
            .map(|entry| usize::from(entry.count))
            .sum::<usize>(),
        30
    );
    assert_eq!(signature.sideboard().len(), 3);

    let selected = catalog
        .select(BuildId(249896), Some(&signature))
        .expect("exact task-hunter match");
    assert_eq!(selected.id(), "standard.quest_hunter.249896");
    assert_eq!(
        selected.selection_reason(),
        &PlaybookSelectionReason::ExactDeckMatch {
            build: BuildId(249896)
        }
    );

    let generic = catalog
        .select(BuildId(249896), None)
        .expect("generic without deck evidence");
    assert_eq!(generic.id(), "generic");
    assert_ne!(
        selected.weight(ScoreFeature::FriendlyBoardAttack),
        generic.weight(ScoreFeature::FriendlyBoardAttack)
    );
    assert_eq!(
        generic.selection_reason(),
        &PlaybookSelectionReason::GenericFallback {
            build: BuildId(249896),
            reason: GenericFallbackReason::DeckSignatureUnavailable,
        }
    );

    let cross_build = catalog
        .select(BuildId(249897), Some(&signature))
        .expect("exact config must not leak across builds");
    assert_eq!(cross_build.id(), "generic");
    assert_eq!(
        cross_build.selection_reason(),
        &PlaybookSelectionReason::GenericFallback {
            build: BuildId(249897),
            reason: GenericFallbackReason::NoExactDeckMatch,
        }
    );
}
