use std::{collections::BTreeMap, sync::Arc, time::SystemTime};

use alloy_consensus::Header;
use alloy_primitives::{keccak256, BlockTimestamp, B256};
use alloy_rpc_client::RpcClient;
use alloy_sol_types::SolValue;
use anyhow::Result;
use tokio::sync::{broadcast, RwLock};
use tokio_util::sync::CancellationToken;

use crate::{
    common::{contract::L2Output, state::State},
    components::game_fetcher::GameFetcherNotification,
    rpc::el,
};

pub struct GameChallenger {
    state: Arc<RwLock<State>>,
    l2_rpc: RpcClient,
}

impl GameChallenger {
    pub fn new(state: Arc<RwLock<State>>, l2_rpc: RpcClient) -> Self {
        Self { state, l2_rpc }
    }

    fn __get_output_root(header: &Header) -> Option<B256> {
        let l2_claim_encoded = L2Output {
            zero: 0,
            l2_state_root: header.state_root,
            l2_storage_hash: header.withdrawals_root?,
            l2_claim_hash: header.hash_slow(),
        };
        Some(keccak256(l2_claim_encoded.abi_encode()))
    }

    pub async fn process(&self, noti: GameFetcherNotification) -> Result<()> {
        let now: BlockTimestamp =
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_secs();

        let output_roots: BTreeMap<_, _> = {
            let block_numbers: Vec<_> = {
                let state = self.state.read().await;
                (noti.fetched_games.keys())
                    .map(|i| state.games.get(i).map(|g| g.claim_block))
                    .flatten()
                    .collect()
            };
            let block_headers = el::get_block_headers(&self.l2_rpc, &block_numbers).await?;
            block_headers
                .iter()
                .filter_map(|header| match Self::__get_output_root(header) {
                    Some(r) => Some((header.number, r)),
                    None => None,
                })
                .collect()
        };

        tracing::info!(?output_roots, "Output roots calculated");

        // TODO: should_challenge_game - parent not in state: If a game's parent is outside
        // fetched_range, the parent check is skipped and only the output root check applies.
        // Could miss challenges where the parent CHALLENGER_WINS but isn't fetched.
        let games_to_challenges: Vec<_> = {
            let mut state = self.state.write().await;
            state.correct_output_roots.extend(output_roots);
            (noti.fetched_games.keys().copied())
                .filter(|i| state.should_challenge_game(*i, now))
                .collect()
        };

        tracing::info!(?games_to_challenges, "Decided games to challenge");

        // TODO: if games_to_challenges is not empty, call challenge() function

        Ok(())
    }

    pub async fn start(
        self,
        ct: CancellationToken,
        mut game_fetcher_broadcast_rx: broadcast::Receiver<GameFetcherNotification>,
    ) {
        loop {
            match ct.run_until_cancelled(game_fetcher_broadcast_rx.recv()).await {
                None => {
                    tracing::info!("Shutdown signal received, stopping");
                    break;
                }
                Some(Err(broadcast::error::RecvError::Closed)) => {
                    tracing::info!("Subscription disconnected, stopping");
                    break;
                }
                Some(Err(broadcast::error::RecvError::Lagged(num_skipped_messages))) => {
                    tracing::info!(%num_skipped_messages, "Lagging detected, continuing");
                    break;
                }
                Some(Ok(noti)) => match self.process(noti).await {
                    Ok(()) => {}
                    Err(err) => {
                        tracing::info!(%err, "Failed to process");
                    }
                },
            }
        }
    }
}
