use std::{collections::BTreeMap, num::NonZeroUsize, sync::Arc, time::SystemTime};

use alloy_consensus::Header;
use alloy_network::Ethereum;
use alloy_primitives::{keccak256, Address, BlockNumber, BlockTimestamp, B256};
use alloy_provider::RootProvider;
use alloy_rpc_client::RpcClient;
use alloy_sol_types::SolValue;
use anyhow::Result;
use tokio::sync::{broadcast, mpsc, oneshot, RwLock};
use tokio_util::sync::CancellationToken;

use crate::{
    common::{
        contract::{L2Output, OPSuccinctFaultDisputeGame},
        primitives::{pick_random_games, GameIndex},
        state::State,
    },
    components::{
        game_fetcher::GameFetcherNotification,
        tx_manager::{TxManagerRequest, TxManagerSenderRole},
    },
    rpc::el,
};

#[derive(Debug, clap::Args)]
pub struct GameChallengerConfig {
    #[arg(id = "challenger.batch-size", long = "challenger.batch-size", default_value = "64")]
    pub batch_size: NonZeroUsize,
}

pub struct GameChallenger {
    config: GameChallengerConfig,
    state: Arc<RwLock<State>>,
    l1_rpc: RpcClient,
    l2_rpc: RpcClient,
    tx_manager_tx: mpsc::Sender<TxManagerRequest>,
}

impl GameChallenger {
    pub fn new(
        config: GameChallengerConfig,
        state: Arc<RwLock<State>>,
        l1_rpc: RpcClient,
        l2_rpc: RpcClient,
        tx_manager_tx: mpsc::Sender<TxManagerRequest>,
    ) -> Self {
        Self { config, state, l1_rpc, l2_rpc, tx_manager_tx }
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

    fn __next_games_to_challenge(
        state: &State,
        now: BlockTimestamp,
    ) -> BTreeMap<GameIndex, Address> {
        (state.games.iter())
            .filter(|(_i, g)| state.should_challenge_game(g, now))
            .map(|(&i, g)| (i, g.game_address))
            .collect()
    }

    async fn refresh_games(&self, games: impl Iterator<Item = GameIndex>) -> Result<()> {
        let block_numbers: Vec<BlockNumber> = {
            let state = self.state.read().await;
            let batch_size = self.config.batch_size.get();

            let mut game_indexes = Vec::with_capacity(batch_size);
            game_indexes.extend(games);
            game_indexes.extend(pick_random_games(
                state.fetched_range.clone(),
                batch_size.saturating_sub(game_indexes.len()),
            ));

            let mut block_numbers: Vec<BlockNumber> = game_indexes
                .iter()
                .filter_map(|i| state.games.get(i).map(|g| g.claim_block))
                .collect();
            block_numbers.sort();
            block_numbers.dedup();
            block_numbers
        };
        tracing::info!(?block_numbers, "Fetching output roots for block numbers");

        let output_roots: BTreeMap<BlockNumber, B256> =
            el::get_block_headers(&self.l2_rpc, &block_numbers)
                .await?
                .iter()
                .filter_map(|h| Self::__get_output_root(h).map(|r| (h.number, r)))
                .collect();
        tracing::info!(?output_roots, "Fetched correct output roots");

        self.state.write().await.correct_output_roots.extend(output_roots);
        Ok(())
    }

    pub async fn process(&self, noti: GameFetcherNotification) -> Result<()> {
        let now: BlockTimestamp =
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_secs();
        self.refresh_games(noti.fetched_games.keys().copied()).await?;

        let next_games_to_challenge: BTreeMap<GameIndex, Address> = {
            let state = self.state.read().await;
            Self::__next_games_to_challenge(&state, now)
        };
        tracing::info!(?next_games_to_challenge, "Decided games to challenge");

        for (game_index, game_address) in next_games_to_challenge {
            let l1_provider = RootProvider::<Ethereum>::new(self.l1_rpc.clone());
            let game = OPSuccinctFaultDisputeGame::new(game_address, l1_provider);
            let challenger_bond = game.challengerBond().call().await?;
            let transaction_request =
                game.challenge().value(challenger_bond).into_transaction_request();
            tracing::info!(%game_index, %game_address, ?transaction_request, "Forwarding challenge transaction");

            let (done_tx, done_rx) = oneshot::channel();
            self.tx_manager_tx
                .send(TxManagerRequest::Send(
                    TxManagerSenderRole::Challenger,
                    transaction_request,
                    done_tx,
                ))
                .await?;
            let receipt = done_rx.await?; // TODO: allow concurrent submissions
            tracing::info!(%game_index, %game_address, ?receipt, "Submitted challenge transaction");

            // TODO: ask game fetcher to sync again
        }

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
                    tracing::warn!("Subscription disconnected, stopping");
                    break;
                }
                Some(Err(broadcast::error::RecvError::Lagged(num_skipped_messages))) => {
                    tracing::warn!(%num_skipped_messages, "Lagging detected, continuing");
                    continue;
                }
                Some(Ok(noti)) => match self.process(noti).await {
                    Ok(()) => {}
                    Err(err) => {
                        tracing::error!(%err, "Failed to process");
                    }
                },
            }
        }
    }
}
