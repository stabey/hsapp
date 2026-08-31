//! 已见友方牌 → 候选牌组匹配(兜底,不自动切换)。

use crate::Deck;
use hs_types::{DbfId, DeckId};
use std::collections::BTreeMap;

/// 按「已见牌被牌组包含的张数」降序返回候选牌组 ID。
///
/// - 包含度 = 已见多重集与牌组主牌多重集的交(sideboard 不参与)。
/// - 包含度为 0 的牌组不返回。
/// - 同分再比:未匹配的已见牌更少者优先,然后主牌总张数更少(更贴),再比 `DeckId`。
/// - 调用方不得据此自动 `set_active`。
pub fn match_decks(seen: &[DbfId], candidates: &[Deck]) -> Vec<DeckId> {
    let seen_counts = counts(seen.iter().copied());
    let seen_total: u32 = seen_counts.values().copied().sum();

    let mut scored: Vec<(u32, u32, u32, DeckId)> = Vec::new();
    for deck in candidates {
        let deck_counts = counts(
            deck.list
                .cards
                .iter()
                .flat_map(|(id, n)| std::iter::repeat_n(*id, usize::from(*n))),
        );
        let contained = intersection(&seen_counts, &deck_counts);
        if contained == 0 {
            continue;
        }
        let unmatched = seen_total.saturating_sub(contained);
        let deck_size: u32 = deck.list.cards.iter().map(|(_, n)| u32::from(*n)).sum();
        scored.push((contained, unmatched, deck_size, deck.id));
    }

    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
            .then(a.3.cmp(&b.3))
    });
    scored.into_iter().map(|row| row.3).collect()
}

fn counts(ids: impl IntoIterator<Item = DbfId>) -> BTreeMap<i64, u32> {
    let mut map = BTreeMap::new();
    for id in ids {
        *map.entry(id.0).or_insert(0) += 1;
    }
    map
}

fn intersection(seen: &BTreeMap<i64, u32>, deck: &BTreeMap<i64, u32>) -> u32 {
    seen.iter()
        .map(|(id, n)| (*n).min(deck.get(id).copied().unwrap_or(0)))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hs_deckstrings::DeckList;
    use hs_types::FormatType;

    fn deck(id: i64, cards: Vec<(i64, u8)>) -> Deck {
        Deck {
            id: DeckId(id),
            name: format!("d{id}"),
            class: "MAGE".into(),
            format: FormatType::Standard,
            list: DeckList {
                format: FormatType::Standard,
                heroes: vec![DbfId(637)],
                cards: cards.into_iter().map(|(id, n)| (DbfId(id), n)).collect(),
                sideboards: vec![],
            },
            archived: false,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn ranks_by_inclusion_and_drops_zero() {
        let a = deck(1, vec![(1, 2), (2, 1)]);
        let b = deck(2, vec![(1, 1)]);
        let c = deck(3, vec![(9, 2)]);
        let seen = [DbfId(1), DbfId(1), DbfId(2)];
        assert_eq!(match_decks(&seen, &[a, b, c]), vec![DeckId(1), DeckId(2)]);
    }

    #[test]
    fn tighter_deck_wins_tie_on_containment() {
        let wide = deck(10, vec![(1, 2), (2, 2), (3, 2)]);
        let tight = deck(11, vec![(1, 1), (2, 1)]);
        let seen = [DbfId(1), DbfId(2)];
        assert_eq!(
            match_decks(&seen, &[wide, tight]),
            vec![DeckId(11), DeckId(10)]
        );
    }
}
