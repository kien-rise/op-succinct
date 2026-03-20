use std::{fmt::Debug, num::NonZeroUsize, ops::Range, str::FromStr, sync::Arc, time::Duration};

use alloy_primitives::{Address, U256};
use alloy_provider::{layers::CallBatchLayer, Provider, ProviderBuilder};
use alloy_rpc_client::RpcClient;
use anyhow::Result;
use fault_proof::contract::{AnchorStateRegistry, DisputeGameFactory, OPSuccinctFaultDisputeGame};
use rand::seq::index::sample;
use tokio::{sync::RwLock, task::JoinSet};
use tokio_util::sync::CancellationToken;

use crate::common::{
    primitives::{parse_duration, AnchorRootSnapshot, GameIndex, GameSnapshot},
    state::State,
};

#[derive(Debug, clap::Args)]
pub struct GameFetcherConfig {
    #[arg(
        long = "fetcher.bounded-range",
        value_parser = GameFetcherConfig::__parse_range::<GameIndex>
    )]
    bounded_range: Option<Range<GameIndex>>, /* TODO: create a custom range type to support
                                              * these: a..b, a.., ..b */
    #[arg(
        long = "fetcher.poll-interval",
        value_parser = parse_duration,
        default_value = "1",
        help = "Polling interval. Examples: 2.5 (seconds), 1m, 500ms, 10s."
    )]
    poll_interval: Duration,
    #[arg(long = "fetcher.batch-size", default_value = "1")]
    batch_size: NonZeroUsize,
}

impl GameFetcherConfig {
    fn __parse_range<T: FromStr<Err: Debug>>(s: &str) -> Result<Range<T>, String> {
        let (start, end) = s.split_once("..").ok_or_else(|| "missing ..".to_string())?;
        let start = start.parse::<T>().map_err(|e| format!("invalid start value: {:?}", e))?;
        let end = end.parse::<T>().map_err(|e| format!("invalid end value: {:?}", e))?;
        Ok(start..end)
    }
}

pub struct GameFetcher {
    state: Arc<RwLock<State>>,
    config: GameFetcherConfig,
    l1_rpc: RpcClient,
    factory_address: Address,
    registry_address: Address,
}

impl GameFetcher {
    pub fn new(
        state: Arc<RwLock<State>>,
        config: GameFetcherConfig,
        l1_rpc_client: RpcClient,
        factory_address: Address,
        registry_address: Address,
    ) -> Self {
        Self { state, config, l1_rpc: l1_rpc_client, factory_address, registry_address }
    }

    fn l1_provider(&self) -> impl Provider + Clone {
        ProviderBuilder::new().layer(CallBatchLayer::new()).connect_client(self.l1_rpc.clone())
    }

    async fn fetch_metadata(&self) -> Result<(u32, AnchorRootSnapshot)> {
        let l1_provider = self.l1_provider();
        let factory_contract = DisputeGameFactory::new(self.factory_address, l1_provider.clone());
        let registry_contract =
            AnchorStateRegistry::new(self.registry_address, l1_provider.clone());
        let (game_count, anchor_root, anchor_game) = l1_provider
            .multicall()
            .add(factory_contract.gameCount())
            .add(registry_contract.getAnchorRoot())
            .add(registry_contract.anchorGame())
            .aggregate()
            .await?;

        let (game_count, anchor_root) = {
            let mut state = self.state.write().await;
            state.game_count = game_count.to();
            state.anchor_root = AnchorRootSnapshot {
                output_root: anchor_root._0,
                claim_block: anchor_root._1.to(),
                anchor_game,
            };
            (state.game_count, state.anchor_root.clone())
        };

        Ok((game_count, anchor_root))
    }

    fn __next_games_to_fetch(
        game_count: u32,
        fetched_range: Range<GameIndex>,
        bounded_range: Option<Range<GameIndex>>,
        batch_size: usize,
    ) -> Vec<GameIndex> {
        let mut next_games = Vec::with_capacity(batch_size);

        let (start, end) = if !fetched_range.is_empty() {
            (fetched_range.start, fetched_range.end)
        } else if let Some(range) = bounded_range.as_ref() {
            let game_index = game_count.clamp(range.start, range.end);
            (game_index, game_index)
        } else {
            (game_count, game_count)
        };

        let mut try_push = |game_index| {
            if next_games.len() < next_games.capacity() &&
                bounded_range.as_ref().is_none_or(|r| r.contains(&game_index))
            {
                next_games.push(game_index);
            }
        };

        // 1. Games after end
        for game_index in (end..game_count).take(batch_size) {
            try_push(game_index);
        }

        // 2. Games before start
        for game_index in (0..start).rev().take(batch_size) {
            try_push(game_index);
        }

        // 3. Games between start and end
        if end - start < batch_size as u32 {
            for game_index in start..end {
                try_push(game_index);
            }
        } else {
            for i in sample(&mut rand::rng(), (end - start) as usize, batch_size) {
                try_push(start + i as u32);
            }
        }

        next_games
    }

    async fn __fetch_game(
        l1_provider: impl Provider + Clone,
        factory_address: Address,
        registry_address: Address,
        game_index: GameIndex,
    ) -> Result<GameSnapshot> {
        let factory = DisputeGameFactory::new(factory_address, l1_provider.clone());
        let (game_address, created_at) = {
            let ret = factory.gameAtIndex(U256::from(game_index)).call().await?;
            (ret.proxy, ret.timestamp)
        };

        let game_contract = OPSuccinctFaultDisputeGame::new(game_address, l1_provider.clone());
        let registry_contract = AnchorStateRegistry::new(registry_address, l1_provider.clone());

        let (output_root, claim_block, start_block, parent_index, game_status, is_blacklisted) =
            l1_provider
                .multicall()
                .add(game_contract.rootClaim())
                .add(game_contract.l2BlockNumber())
                .add(game_contract.startingBlockNumber())
                .add(game_contract.parentIndex())
                .add(game_contract.status())
                .add(registry_contract.isGameBlacklisted(game_address))
                .aggregate()
                .await?;

        let game_snapshot = GameSnapshot {
            proxy_address: game_address,
            output_root,
            claim_block: claim_block.to(),
            created_at,
            start_block: start_block.to(), // not in IDisputeGame
            parent_index,                  // not in IDisputeGame
            game_status,
            is_blacklisted,
        };

        Ok(game_snapshot)
    }

    async fn fetch_new_games(&self) -> Result<Vec<GameIndex>> {
        let (fetched_range, game_count) = {
            let state = self.state.read().await;
            (state.fetched_range.clone(), state.game_count)
        };

        let next_games_to_fetch = Self::__next_games_to_fetch(
            game_count,
            fetched_range,
            self.config.bounded_range.clone(),
            self.config.batch_size.get(),
        );

        let mut set = JoinSet::new();

        for game_index in next_games_to_fetch {
            let l1_provider = self.l1_provider();
            let factory_address = self.factory_address;
            let registry_address = self.registry_address;
            let state = self.state.clone();
            set.spawn(async move {
                match Self::__fetch_game(l1_provider, factory_address, registry_address, game_index)
                    .await
                {
                    Ok(game_snapshot) => {
                        tracing::info!(game_index, ?game_snapshot, "Fetched game");
                        let changed = state.write().await.insert_game(game_index, game_snapshot);
                        changed.then_some(game_index)
                    }
                    Err(err) => {
                        tracing::warn!(game_index, ?err, "Fetch game failed");
                        None
                    }
                }
            });
        }

        let results = set.join_all().await;
        let mut changed_games: Vec<_> = results.into_iter().flatten().collect();
        changed_games.sort();

        Ok(changed_games)
    }

    pub async fn start(self, ct: CancellationToken) {
        let mut has_made_progress = true;

        while !ct.is_cancelled() {
            if !has_made_progress {
                ct.run_until_cancelled(tokio::time::sleep(self.config.poll_interval)).await;
            }
            has_made_progress = false;

            match self.fetch_metadata().await {
                Ok((game_count, anchor_root)) => {
                    tracing::info!(%game_count, ?anchor_root, "Metadata updated")
                }
                Err(err) => {
                    tracing::warn!(%err, "Failed to fetch metadata");
                    continue;
                }
            };

            match self.fetch_new_games().await {
                Ok(changed_games) => {
                    if changed_games.is_empty() {
                        tracing::info!("Tried fetching games but no changes found");
                        continue;
                    } else {
                        tracing::info!(?changed_games, "Fetched games");
                        has_made_progress = true;
                        continue;
                    }
                }
                Err(err) => {
                    tracing::error!(%err, "Failed to fetch games");
                    continue;
                }
            }
        }
    }
}
