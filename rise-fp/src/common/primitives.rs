use std::fmt::Debug;

use alloy_primitives::{Address, BlockNumber, B256};
use fault_proof::contract::GameStatus;

pub type GameIndex = u32;

#[derive(Debug, PartialEq)]
pub struct GameSnapshot {
    pub proxy_address: Address,
    pub output_root: B256,
    pub claim_block: BlockNumber,
    pub start_block: BlockNumber,
    pub game_status: GameStatus,
}
