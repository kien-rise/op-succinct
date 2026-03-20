use std::{sync::Arc, time::Duration};

use alloy_primitives::Address;
use alloy_rpc_client::RpcClient;
use anyhow::Result;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::common::{primitives::parse_duration, state::State};

#[derive(Debug, clap::Args)]
pub struct GameCreatorConfig {
    #[arg(
        id = "creator.poll-interval",
        long = "creator.poll-interval",
        value_parser = parse_duration,
        default_value = "1",
        help = "Polling interval. Examples: 2.5 (seconds), 1m, 500ms, 10s."
    )]
    poll_interval: Duration,
}

pub struct GameCreator {
    state: Arc<RwLock<State>>,
    config: GameCreatorConfig,
    l1_rpc: RpcClient,
    factory_address: Address,
}

impl GameCreator {
    pub fn new(
        state: Arc<RwLock<State>>,
        config: GameCreatorConfig,
        l1_rpc: RpcClient,
        factory_address: Address,
    ) -> Self {
        Self { state, config, l1_rpc, factory_address }
    }

    async fn step(&self) -> Result<bool> {
        let canonical_game_index = {
            let state = self.state.read().await;
            state.get_canonical_game_index()
        };
        tracing::info!(?canonical_game_index, "step");
        Ok(false)
    }

    pub async fn start(self, ct: CancellationToken) {
        while !ct.is_cancelled() {
            match self.step().await {
                Ok(progress) => {
                    if !progress {
                        ct.run_until_cancelled(tokio::time::sleep(self.config.poll_interval)).await;
                    }
                }
                Err(err) => {
                    tracing::warn!(%err, "Failed to step");
                }
            }
        }
    }
}

// pub async fn get_next_games_to_create(
//     l1_rpc: &RpcClient,
//     l2_rpc: &RpcClient,
//     cl_rpc: &RpcClient,
//     anchor_state_registry_address: Address,
//     dispute_game_factory_address: Address,
//     starting_game_index: GameIndex,
//     proposal_interval_in_blocks: u64,
//     limit: usize,
// ) -> Result<Vec<BlockNumber>> {
//     tracing::debug!(
//         starting_game_index,
//         proposal_interval_in_blocks,
//         ?limit,
//         "get_next_games_to_create: start"
//     );

//     // TODO: check respected game type

//     let starting_l2_block_number = get_l2_block_number_at_game_index(
//         l1_rpc,
//         anchor_state_registry_address,
//         dispute_game_factory_address,
//         starting_game_index,
//     )
//     .await?;

//     let l1_finalized_block_number = get_finalized_block_number(l1_rpc).await?;
//     let l2_finalized_block_number = get_finalized_block_number(l2_rpc).await?;

//     if starting_l2_block_number > l2_finalized_block_number {
//         bail!("starting L2 block is ahead of the finalized L2 block");
//     }

//     let mut current_l2_block_number = starting_l2_block_number;
//     let mut l2_block_numbers = Vec::new();

//     while l2_block_numbers.len() < limit {
//         let next_l2_block_number = if proposal_interval_in_blocks > 0 {
//             current_l2_block_number + proposal_interval_in_blocks
//         } else if let Some(l1_block_number) = get_safe_l1_block_for_l2_block(
//             cl_rpc,
//             l1_finalized_block_number,
//             current_l2_block_number + 1,
//         )
//         .await?
//         {
//             let l2_safe_head = optimism_safeHeadAtL1Block(cl_rpc, l1_block_number).await?;
//             l2_safe_head.safe_head.number
//         } else {
//             BlockNumber::MAX
//         };

//         if next_l2_block_number <= current_l2_block_number {
//             bail!("next L2 block number must strictly increase from the current block");
//         } else if next_l2_block_number > l2_finalized_block_number {
//             break;
//         }

//         tracing::debug!(next_l2_block_number, "get_next_games_to_create: append");
//         l2_block_numbers.push(next_l2_block_number);
//         current_l2_block_number = next_l2_block_number;
//     }

//     tracing::debug!(?l2_block_numbers, "get_next_games_to_create: end");
//     Ok(l2_block_numbers)
// }
