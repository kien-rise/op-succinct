use std::{fmt::Debug, num::NonZeroUsize, ops::Range, sync::Arc, time::Duration};

use alloy_primitives::{Address, U256};
use alloy_provider::{layers::CallBatchLayer, Provider, ProviderBuilder};
use alloy_rpc_client::RpcClient;
use anyhow::Result;
use fault_proof::contract::{AnchorStateRegistry, DisputeGameFactory, OPSuccinctFaultDisputeGame};
use rand::seq::index::sample;
use tokio::{
    sync::{mpsc, oneshot, Notify, RwLock},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use crate::common::{
    primitives::{parse_duration, parse_range, AnchorRootSnapshot, GameIndex, GameSnapshot},
    state::State,
};

#[derive(Debug, clap::Args)]
pub struct GameFetcherConfig {
    // TODO: create a custom range type to support these: a..b, a.., ..b
    #[arg(
        long = "fetcher.bounded-range",
        value_parser = parse_range::<GameIndex>
    )]
    pub bounded_range: Option<Range<GameIndex>>,

    #[arg(
        id = "fetcher.poll-interval",
        long = "fetcher.poll-interval",
        value_parser = parse_duration,
        default_value = "1",
        help = "Polling interval. Examples: 2.5 (seconds), 1m, 500ms, 10s."
    )]
    pub poll_interval: Duration,

    #[arg(id = "fetcher.batch-size", long = "fetcher.batch-size", default_value = "16")]
    pub batch_size: NonZeroUsize,
}

#[derive(Debug)]
pub enum GameFetcherRequest {
    // request to sync, returns a bool indicating if any changes happens
    Step(oneshot::Sender<bool>),
    // more types will be added later
}

impl GameFetcherRequest {
    pub fn channel() -> (mpsc::Sender<Self>, mpsc::Receiver<Self>) {
        mpsc::channel(256)
    }
}

pub struct GameFetcher {
    state: Arc<RwLock<State>>,
    config: GameFetcherConfig,
    l1_rpc: RpcClient,
    factory_address: Address,
    registry_address: Address,
    notification: Arc<Notify>,
}

impl GameFetcher {
    pub fn new(
        state: Arc<RwLock<State>>,
        config: GameFetcherConfig,
        l1_rpc_client: RpcClient,
        factory_address: Address,
        registry_address: Address,
        notification: Arc<Notify>,
    ) -> Self {
        Self {
            state,
            config,
            l1_rpc: l1_rpc_client,
            factory_address,
            registry_address,
            notification,
        }
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

    async fn dispatch(&self, request: GameFetcherRequest) -> Result<()> {
        match request {
            GameFetcherRequest::Step(done) => {
                let (game_count, anchor_root) = self.fetch_metadata().await?;
                tracing::info!(%game_count, ?anchor_root, "Metadata updated");
                let changed_games = self.fetch_new_games().await?;
                tracing::info!(?changed_games, "Fetched games");
                let _ = done.send(!changed_games.is_empty()); // returns whether progress has been made
                self.notification.notify_waiters();
            }
        }
        Ok(())
    }

    pub async fn start_driver(
        ct: CancellationToken,
        tx: mpsc::Sender<GameFetcherRequest>,
        poll_interval: Duration,
    ) {
        let mut immediate = true;

        loop {
            let delay = if immediate { Duration::ZERO } else { poll_interval };
            if ct.run_until_cancelled(tokio::time::sleep(delay)).await.is_none() {
                tracing::info!("Shutdown signal received, stopping");
                break;
            }

            let (t, r) = oneshot::channel();
            if tx.send(GameFetcherRequest::Step(t)).await.is_err() {
                tracing::info!("Channel closed, stopping");
                break;
            };

            immediate = if let Ok(has_progress) = r.await { has_progress } else { false }
        }
    }

    pub async fn start_dispatcher(
        self,
        ct: CancellationToken,
        mut rx: mpsc::Receiver<GameFetcherRequest>,
    ) {
        loop {
            match ct.run_until_cancelled(rx.recv()).await {
                Some(Some(request)) => {
                    tracing::debug!(?request, "Handling request");
                    if let Err(err) = self.dispatch(request).await {
                        tracing::error!(%err, "Failed to handle request");
                    }
                }
                Some(None) => {
                    tracing::info!("Channel closed, stopping");
                    break;
                }
                None => {
                    tracing::info!("Shutdown signal received, stopping");
                    break;
                }
            }
        }
    }
}
