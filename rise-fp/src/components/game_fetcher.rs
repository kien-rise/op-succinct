use std::{
    collections::BTreeMap,
    fmt::Debug,
    num::NonZeroUsize,
    ops::{Range, RangeBounds},
    sync::Arc,
    time::Duration,
};

use alloy_primitives::{Address, U256};
use alloy_provider::{layers::CallBatchLayer, Provider, ProviderBuilder};
use alloy_rpc_client::RpcClient;
use anyhow::Result;
use rand::seq::index::sample;
use tokio::{
    sync::{broadcast, mpsc, oneshot, RwLock},
    task::JoinSet,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

use crate::common::{
    contract::{AnchorStateRegistry, DisputeGameFactory, OPSuccinctFaultDisputeGame},
    primitives::{parse_duration, AnchorRootSnapshot, GameIndex, GameRangeInclusive, GameSnapshot},
    state::State,
};

#[derive(Debug, clap::Args)]
pub struct GameFetcherConfig {
    #[arg(long = "fetcher.first-game-index")]
    pub first_game_index: Option<GameIndex>,

    #[arg(long = "fetcher.last-game-index")]
    pub last_game_index: Option<GameIndex>,

    #[arg(long = "fetcher.poll-interval", value_parser = parse_duration, default_value = "60")]
    pub poll_interval: Duration,

    #[arg(id = "fetcher.batch-size", long = "fetcher.batch-size", default_value = "16")]
    pub batch_size: NonZeroUsize,

    #[arg(
        id = "fetcher.broadcast-channel-capacity",
        long = "fetcher.broadcast-channel-capacity",
        default_value = "256"
    )]
    pub broadcast_channel_capacity: NonZeroUsize,
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

#[derive(Debug, Clone)]
pub struct GameFetcherNotification {
    pub fetched_games: BTreeMap<GameIndex, bool>,
}

pub struct GameFetcher {
    state: Arc<RwLock<State>>,
    config: GameFetcherConfig,
    l1_rpc: RpcClient,
    factory_address: Address,
    registry_address: Address,

    broadcast_tx: broadcast::Sender<GameFetcherNotification>,
}

impl GameFetcher {
    pub fn new(
        config: GameFetcherConfig,
        state: Arc<RwLock<State>>,
        l1_rpc_client: RpcClient,
        factory_address: Address,
        registry_address: Address,
    ) -> Self {
        let (broadcast_tx, _broadcast_rx) =
            broadcast::channel(config.broadcast_channel_capacity.get());

        Self {
            config,
            state,
            l1_rpc: l1_rpc_client,
            factory_address,
            registry_address,

            broadcast_tx,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<GameFetcherNotification> {
        self.broadcast_tx.subscribe()
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
        bounded_range: GameRangeInclusive,
        batch_size: usize,
    ) -> Vec<GameIndex> {
        let mut next_games = Vec::with_capacity(batch_size);

        let (start, end) = if !fetched_range.is_empty() {
            (fetched_range.start, fetched_range.end)
        } else {
            let game_index = bounded_range.clamp(game_count);
            (game_index, game_index)
        };

        let mut try_push = |game_index| {
            if next_games.len() < next_games.capacity() && bounded_range.contains(&game_index) {
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
        use alloy_provider::MulticallItem; // for .into_call()

        let factory = DisputeGameFactory::new(factory_address, l1_provider.clone());
        let (game_type, created_at, game_address) =
            factory.gameAtIndex(U256::from(game_index)).call().await?.into();

        let game_contract = OPSuccinctFaultDisputeGame::new(game_address, l1_provider.clone());
        let registry_contract = AnchorStateRegistry::new(registry_address, l1_provider.clone());

        #[allow(non_snake_case)]
        let (
            rootClaim,
            l2BlockNumber,
            status,
            isGameBlacklisted,
            startingBlockNumber,
            parentIndex,
            claimData,
        ) = l1_provider
            .multicall()
            .add(game_contract.rootClaim())
            .add(game_contract.l2BlockNumber())
            .add(game_contract.status())
            .add(registry_contract.isGameBlacklisted(game_address))
            .add_call(game_contract.startingBlockNumber().into_call(true))
            .add_call(game_contract.parentIndex().into_call(true))
            .add_call(game_contract.claimData().into_call(true))
            .aggregate3()
            .await?;

        let game_snapshot = GameSnapshot {
            game_type,
            created_at,
            game_address,

            // IDisputeGame
            output_root: rootClaim?,
            claim_block: l2BlockNumber?.to(),
            game_status: status?,
            is_blacklisted: isGameBlacklisted?,

            // OPSuccinctFaultDisputeGame only
            // These calls may fail when the game type is not 42.
            // We tolerate such errors by falling back to default values.
            // If the game type isn't 42, these fields are irrelevant.
            start_block: startingBlockNumber.unwrap_or_default().to(),
            parent_index: parentIndex.unwrap_or_default(),
            claim_data: claimData.unwrap_or_default(),
        };

        Ok(game_snapshot)
    }

    async fn fetch_new_games(&self) -> Result<BTreeMap<GameIndex, bool>> {
        let (fetched_range, game_count) = {
            let state = self.state.read().await;
            (state.fetched_range.clone(), state.game_count)
        };

        let next_games_to_fetch = Self::__next_games_to_fetch(
            game_count,
            fetched_range,
            GameRangeInclusive::new(self.config.first_game_index, self.config.last_game_index),
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
                        Some((game_index, changed))
                    }
                    Err(err) => {
                        tracing::warn!(game_index, ?err, "Fetch game failed");
                        None
                    }
                }
            });
        }

        // TODO: also use eth_subscribe to watch for game events

        let results = set.join_all().await;
        let updated_games: BTreeMap<_, _> = results.into_iter().flatten().collect();

        Ok(updated_games)
    }

    async fn dispatch(&self, request: GameFetcherRequest) -> Result<()> {
        match request {
            GameFetcherRequest::Step(done) => {
                let (game_count, anchor_root) = self.fetch_metadata().await?;
                tracing::info!(%game_count, ?anchor_root, "Metadata updated");
                let fetched_games = self.fetch_new_games().await?;
                tracing::info!(?fetched_games, "Fetched games");
                let has_progress = fetched_games.values().any(|changed| *changed);
                let _ = done.send(has_progress);
                let _ = self.broadcast_tx.send(GameFetcherNotification { fetched_games });
            }
        }
        Ok(())
    }

    pub async fn start(self, ct: CancellationToken, mut rx: mpsc::Receiver<GameFetcherRequest>) {
        let mut immediate = true;
        loop {
            let delay = if immediate { Duration::ZERO } else { self.config.poll_interval };
            immediate = false;

            match ct.run_until_cancelled(timeout(delay, rx.recv())).await {
                None => {
                    tracing::info!("Shutdown signal received, stopping");
                    break;
                }
                Some(Err(_elapsed)) => {
                    tracing::debug!("Handling idle");
                    let (done_tx, done_rx) = oneshot::channel();
                    if let Err(err) = self.dispatch(GameFetcherRequest::Step(done_tx)).await {
                        tracing::error!(%err, "Failed to handle request");
                    }
                    if let Ok(progress) = done_rx.await {
                        if progress {
                            immediate = true;
                        }
                    }
                }
                Some(Ok(None)) => {
                    tracing::info!("Channel closed, stopping");
                    break;
                }
                Some(Ok(Some(request))) => {
                    tracing::debug!(?request, "Handling request");
                    if let Err(err) = self.dispatch(request).await {
                        tracing::error!(%err, "Failed to handle request");
                    }
                }
            }
        }
    }
}
