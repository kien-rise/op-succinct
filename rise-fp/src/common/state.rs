use std::{collections::BTreeMap, fmt::Debug, ops::Range};

use alloy_primitives::BlockTimestamp;

use crate::common::primitives::{AnchorRootSnapshot, GameIndex, GameSnapshot};

#[derive(Debug, Default)]
pub struct State {
    pub game_count: u32,
    pub games: BTreeMap<GameIndex, GameSnapshot>,
    pub fetched_range: Range<GameIndex>,
    pub anchor_root: AnchorRootSnapshot,
    pub retirement_timestamp: BlockTimestamp,
}

impl State {
    pub fn insert_game(&mut self, game_index: GameIndex, game_snapshot: GameSnapshot) -> bool {
        use std::collections::btree_map::Entry;
        match self.games.entry(game_index) {
            Entry::Vacant(vac) => {
                vac.insert(game_snapshot);
            }
            Entry::Occupied(mut occ) => {
                if occ.get() == &game_snapshot {
                    return false; // no changes found
                } else {
                    occ.insert(game_snapshot);
                }
            }
        }

        if self.fetched_range.is_empty() {
            self.fetched_range = game_index..game_index + 1;
        } else if game_index == self.fetched_range.end {
            while self.games.contains_key(&self.fetched_range.end) {
                self.fetched_range.end += 1;
            }
        } else if game_index + 1 == self.fetched_range.start {
            while (self.fetched_range.start.checked_sub(1))
                .is_some_and(|index| self.games.contains_key(&index))
            {
                self.fetched_range.start -= 1;
            }
        }

        true
    }

}
