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

    fn range_of_possible_canonical_head(&self) -> Option<Range<GameIndex>> {
        if self.fetched_range.end != self.game_count {
            return None; // need to fetch latest games
        }
        if self.anchor_root.is_empty() {
            return None; // need to fetch anchor root
        }
        let fetched_range = self.fetched_range.clone();
        for (&game_index, game_snapshot) in self.games.range(fetched_range).rev() {
            if game_snapshot.is_retired(self.retirement_timestamp) {
                return Some(game_index + 1..self.game_count); // latest retired game found
            }
            if game_snapshot.proxy_address == self.anchor_root.anchor_game {
                return Some(game_index..self.game_count); // anchor game found
            }
        }
        if self.fetched_range.start == 0 {
            return Some(0..self.game_count); // earliest game already found
        }
        None // need to fetch earlier games
    }

    pub fn get_canonical_game_index(&self) -> Option<GameIndex> {
        // 1. Determine the search range; early exit if more games have to be fetched.
        let possible_range = self.range_of_possible_canonical_head()?;
        if possible_range.is_empty() {
            return Some(GameIndex::MAX);
        }

        // 2. Locate the anchor game index within the fetched range.
        let anchor_game_index = (self.games.range(possible_range.clone()))
            .find(|(_, game_snapshot)| {
                game_snapshot.proxy_address == self.anchor_root.anchor_game &&
                    game_snapshot.still_good(self.retirement_timestamp)
            })
            .map(|(game_index, _)| *game_index);

        // 3. Traverse the graph (DFS-style), starting from:
        //    - the anchor game if it exists, or
        //    - root games (no parent) matching the anchor claim block otherwise.
        let mut visited = BTreeMap::new();
        for (game_index, game_snapshot) in self.games.range(possible_range) {
            if !game_snapshot.still_good(self.retirement_timestamp) {
                continue;
            }
            let is_starting_point = match anchor_game_index {
                Some(a) => game_index == &a,
                None => {
                    game_snapshot.parent_index == GameIndex::MAX &&
                        game_snapshot.start_block == self.anchor_root.claim_block
                }
            };
            let is_parent_visited = !is_starting_point &&
                game_snapshot.parent_index != GameIndex::MAX &&
                visited.contains_key(&game_snapshot.parent_index);
            if is_starting_point || is_parent_visited {
                visited.insert(game_index, game_snapshot.claim_block);
            }
        }

        // 4. From the visited set, select the game with the highest claim_block.
        let mut highest_claim_block = self.anchor_root.claim_block;
        let mut corresponding_game_index = GameIndex::MAX;

        for (game_index, claim_block) in visited {
            if claim_block > highest_claim_block {
                highest_claim_block = claim_block;
                corresponding_game_index = *game_index;
            }
        }

        Some(corresponding_game_index)
    }
}
