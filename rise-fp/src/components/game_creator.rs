use std::{collections::BTreeMap, iter::zip, num::NonZeroUsize, ops::Range, sync::Arc};

use alloy_eips::BlockNumberOrTag;
use alloy_primitives::{Address, BlockNumber, Bytes, U256};
use alloy_provider::{layers::CallBatchLayer, Provider, ProviderBuilder};
use alloy_rpc_client::RpcClient;
use alloy_sol_types::{SolEvent, SolValue};
use anyhow::{bail, Result};
use tokio::sync::{broadcast, mpsc, oneshot, RwLock};
use tokio_util::sync::CancellationToken;

use crate::{
    common::{
        contract::DisputeGameFactory::{self, DisputeGameCreated},
        primitives::GameIndex,
        state::State,
    },
    components::{
        game_fetcher::GameFetcherNotification,
        tx_manager::{TxManagerRequest, TxManagerSenderRole},
    },
    rpc::{
        cl::{get_l1_origin, OutputResponse, SafeDBClient},
        el::get_block_header,
        utils::batch_call,
    },
};

#[derive(Debug, clap::Args)]
pub struct GameCreatorConfig {
    #[arg(id = "creator.game-type", long = "creator.game-type", default_value = "42")]
    pub game_type: u32,

    #[arg(
        id = "creator.create-batch-size",
        long = "creator.create-batch-size",
        default_value = "4"
    )]
    pub create_batch_size: NonZeroUsize,

    #[arg(
        id = "creator.safedb-query-batch-size",
        long = "creator.safedb-query-batch-size",
        default_value = "64"
    )]
    pub safedb_query_batch_size: NonZeroUsize,
}

pub struct GameCreator {
    state: Arc<RwLock<State>>,
    config: GameCreatorConfig,
    l1_rpc: RpcClient,
    l2_rpc: RpcClient,
    cl_rpc: RpcClient,
    factory_address: Address,
    tx_manager_tx: mpsc::Sender<TxManagerRequest>,
}

impl GameCreator {
    pub fn new(
        state: Arc<RwLock<State>>,
        config: GameCreatorConfig,
        l1_rpc: RpcClient,
        l2_rpc: RpcClient,
        cl_rpc: RpcClient,
        factory_address: Address,
        tx_manager_tx: mpsc::Sender<TxManagerRequest>,
    ) -> Self {
        Self { state, config, l1_rpc, l2_rpc, cl_rpc, factory_address, tx_manager_tx }
    }

    fn l1_provider(&self) -> impl Provider + Clone {
        ProviderBuilder::new().layer(CallBatchLayer::new()).connect_client(self.l1_rpc.clone())
    }

    fn __range_of_possible_canonical_head(state: &State) -> Option<Range<GameIndex>> {
        if state.fetched_range.end != state.game_count {
            return None; // need to fetch latest games
        }
        if state.anchor_root.is_empty() {
            return None; // need to fetch anchor root
        }
        let fetched_range = state.fetched_range.clone();
        for (&game_index, game_snapshot) in state.games.range(fetched_range).rev() {
            if game_snapshot.is_retired(state.retirement_timestamp) {
                return Some(game_index + 1..state.game_count); // latest retired game found
            }
            if game_snapshot.game_address == state.anchor_root.anchor_game {
                return Some(game_index..state.game_count); // anchor game found
            }
        }
        if state.fetched_range.start == 0 {
            return Some(0..state.game_count); // earliest game already found
        }
        None // need to fetch earlier games
    }

    // Finds the canonical head, which is the starting point for new games
    fn __get_canonical_head(state: &State) -> Option<(GameIndex, BlockNumber)> {
        // 1. Determine the search range; early exit if more games have to be fetched.
        let possible_range = Self::__range_of_possible_canonical_head(state)?;
        if possible_range.is_empty() {
            return Some((GameIndex::MAX, state.anchor_root.claim_block));
        }

        // 2. Locate the anchor game index within the fetched range.
        let anchor_game_index = (state.games.range(possible_range.clone()))
            .find(|(_, game_snapshot)| {
                game_snapshot.game_address == state.anchor_root.anchor_game &&
                    game_snapshot.still_good(state.retirement_timestamp)
            })
            .map(|(game_index, _)| *game_index);

        // 3. Traverse the graph (DFS-style), starting from:
        //    - the anchor game if it exists, or
        //    - root games (no parent) matching the anchor claim block otherwise.
        let mut visited = BTreeMap::new();
        for (&game_index, game_snapshot) in state.games.range(possible_range) {
            if !game_snapshot.still_good(state.retirement_timestamp) {
                continue;
            }
            let is_starting_point = match anchor_game_index {
                Some(a) => game_index == a,
                None => {
                    game_snapshot.parent_index == GameIndex::MAX &&
                        game_snapshot.start_block == state.anchor_root.claim_block
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
        Some(
            visited
                .into_iter()
                .max_by_key(|&(game_index, claim_block)| (claim_block, game_index))
                .unwrap_or((GameIndex::MAX, state.anchor_root.claim_block)),
        )
    }

    async fn next_games_to_create(
        &self,
        canonical_claim_block: BlockNumber,
        max_games_to_create: usize,
    ) -> Result<Vec<BlockNumber>> {
        let l1_finalized =
            get_block_header(&self.l1_rpc, BlockNumberOrTag::Finalized).await?.number;
        let l2_finalized =
            get_block_header(&self.l2_rpc, BlockNumberOrTag::Finalized).await?.number;
        let mut safe_db_client =
            SafeDBClient::new(self.cl_rpc.clone(), self.config.safedb_query_batch_size.get());
        let canonical_l1_origin = get_l1_origin(&self.cl_rpc, canonical_claim_block).await?;
        let l1_block_range = canonical_l1_origin.number..l1_finalized;

        let mut games = Vec::with_capacity(max_games_to_create);
        let mut current = canonical_claim_block + 1;
        while current <= l2_finalized && games.len() < max_games_to_create {
            let l1_safe =
                safe_db_client.l2_to_l1_safe_within_range(current, l1_block_range.clone()).await?;
            let l2_safe = safe_db_client.l1_to_l2_safe(l1_safe, Some(current)).await?;
            games.push(l2_safe);
            current = l2_safe + 1;
        }
        Ok(games)
    }

    async fn create_games(
        &self,
        canonical_game_index: GameIndex,
        next_claim_blocks: &[BlockNumber],
    ) -> Result<()> {
        let l1_provider = self.l1_provider();
        let factory = DisputeGameFactory::new(self.factory_address, &l1_provider);

        // 1. Pre-fetch all output roots in one go
        let output_roots = batch_call(
            &self.cl_rpc,
            "optimism_outputAtBlock",
            next_claim_blocks.iter().map(|bn| (BlockNumberOrTag::Number(*bn),)),
            |resp: OutputResponse| resp.output_root,
        )
        .await?;

        let mut parent_game_index = canonical_game_index;
        for (bn, output_root) in zip(next_claim_blocks, output_roots) {
            let extra_data = Bytes::from((U256::from(*bn), parent_game_index).abi_encode_packed());

            // 2. Check existence and get bond in one multicall
            let (init_game_count, init_bond, existing_game) = l1_provider
                .multicall()
                .add(factory.gameCount())
                .add(factory.initBonds(self.config.game_type))
                .add(factory.games(self.config.game_type, output_root, extra_data.clone()))
                .aggregate()
                .await?;

            if !existing_game.proxy_.is_zero() {
                tracing::warn!(?existing_game, %bn, %parent_game_index, "Skipping creating game");
                continue;
            }

            // 3. Dispatch Transaction
            let transaction_request = factory
                .create(self.config.game_type, output_root, extra_data)
                .value(init_bond)
                .into_transaction_request();

            tracing::info!(?transaction_request, "sending transaction");

            let (done_tx, done_rx) = oneshot::channel();
            self.tx_manager_tx
                .send(TxManagerRequest::Send(
                    TxManagerSenderRole::Proposer,
                    transaction_request,
                    done_tx,
                ))
                .await?;
            let receipt = done_rx.await?;
            tracing::info!(?receipt, %bn, %parent_game_index, "transaction confirmed");

            // 4. Update parent_index for the next iteration
            let game_address: Address = match (receipt.inner.logs().iter())
                .find(|log| log.topic0() == Some(&DisputeGameCreated::SIGNATURE_HASH))
            {
                Some(log) => DisputeGameCreated::decode_log(&log.inner)?.disputeProxy,
                None => bail!("cannot find DisputeGameCreated event"),
            };

            let new_game_index: GameIndex = {
                let last_game_count = factory.gameCount().call().await?;
                let start: GameIndex = init_game_count.to();
                let end: GameIndex = last_game_count.to();

                if end == start + 1 {
                    start
                } else {
                    let games = (l1_provider.multicall().dynamic())
                        .extend((start..end).map(|i| factory.gameAtIndex(U256::from(i))))
                        .aggregate()
                        .await?;
                    zip(start..end, games)
                        .find_map(|(i, g)| (g.proxy_ == game_address).then_some(i))
                        .ok_or_else(|| anyhow::anyhow!("cannot find created game"))?
                }
            };

            tracing::info!(%game_address, %new_game_index, %bn, "Game created");

            parent_game_index = new_game_index;
        }

        Ok(())
    }

    async fn step(&self) -> Result<()> {
        let canonical_head = {
            let state = self.state.read().await;
            Self::__get_canonical_head(&state)
        };
        let Some((canonical_game_index, canonical_claim_block)) = canonical_head else {
            return Ok(())
        };
        let next_claim_blocks = self
            .next_games_to_create(canonical_claim_block, self.config.create_batch_size.get())
            .await?;

        tracing::info!(%canonical_game_index, %canonical_claim_block, ?next_claim_blocks, "step");

        self.create_games(canonical_game_index, &next_claim_blocks).await?;

        // TODO: after create all the games, we must ask (and wait for) the fetcher to sync once

        Ok(())
    }

    pub async fn start(
        self,
        ct: CancellationToken,
        mut game_fetcher_broadcast_rx: broadcast::Receiver<GameFetcherNotification>,
    ) {
        while let Some(notification_result) =
            ct.run_until_cancelled(game_fetcher_broadcast_rx.recv()).await
        {
            tracing::debug!(?notification_result, "Receive GameFetcher notification");
            match self.step().await {
                Ok(_) => {}
                Err(err) => tracing::error!(%err, "Failed to step"),
            }
        }
    }
}
