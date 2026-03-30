use std::{collections::BTreeMap, fmt::Debug, ops::Range};

use alloy_primitives::{BlockNumber, BlockTimestamp, B256};

use crate::common::{
    contract::GameStatus,
    primitives::{AnchorRootSnapshot, GameIndex, GameSnapshot},
};

#[derive(Debug, Default)]
pub struct State {
    pub game_count: u32,
    pub games: BTreeMap<GameIndex, GameSnapshot>,
    pub fetched_range: Range<GameIndex>,
    pub anchor_root: AnchorRootSnapshot,
    pub retirement_timestamp: BlockTimestamp,
    pub correct_output_roots: BTreeMap<BlockNumber, B256>,
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

    // Check if challenging this game will surely win us the bond.
    // See contracts/src/fp/OPSuccinctFaultDisputeGame.sol#resolve()
    pub fn should_challenge_game(&self, game: &GameSnapshot, now: BlockTimestamp) -> bool {
        if !game.can_challenge(now) {
            return false;
        }
        if !game.still_good(self.retirement_timestamp) {
            return false;
        }
        if let Some(parent) = self.games.get(&game.parent_index) {
            if parent.game_status == GameStatus::CHALLENGER_WINS {
                return true;
            }
        }
        if let Some(r) = self.correct_output_roots.get(&game.claim_block) {
            if r != &game.output_root {
                return true;
            }
        }
        false
    }
}
