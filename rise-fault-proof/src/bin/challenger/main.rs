use std::{fmt::Debug, sync::Arc};

use alloy_rpc_client::RpcClient;
use anyhow::Result;
use clap::Parser;
use tokio::{signal, sync::RwLock};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tracing_subscriber::EnvFilter;

use rise_fault_proof::{
    common::{
        args::{ClRpcArgs, L1RpcArgs, L2RpcArgs},
        primitives::RpcArgs,
        state::State,
    },
    components::{
        game_challenger::{GameChallenger, GameChallengerConfig},
        game_fetcher::{GameFetcher, GameFetcherConfig, GameFetcherRequest},
        tx_manager::{TxManager, TxManagerConfig, TxManagerRequest},
    },
    rpc::{cl, el},
};

#[derive(Debug, clap::Parser)]
struct Args {
    #[command(flatten)]
    l1_rpc_args: L1RpcArgs,
    #[command(flatten)]
    l2_rpc_args: L2RpcArgs,
    #[command(flatten)]
    cl_rpc_args: ClRpcArgs,
    #[command(flatten)]
    game_fetcher_config: GameFetcherConfig,
    #[command(flatten)]
    game_challenger_config: GameChallengerConfig,
    #[command(flatten)]
    tx_manager_config: TxManagerConfig,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(match std::env::var(EnvFilter::DEFAULT_ENV) {
            Ok(var) => EnvFilter::builder().parse(var)?,
            Err(_) => EnvFilter::new("info"),
        })
        .init();

    // Create RPC clients
    let l1_rpc: RpcClient = RpcArgs::from(args.l1_rpc_args).into();
    let l2_rpc: RpcClient = RpcArgs::from(args.l2_rpc_args).into();
    let cl_rpc: RpcClient = RpcArgs::from(args.cl_rpc_args).into();

    // Create states, channels, notifications
    let state = Arc::new(RwLock::new(State::default()));
    let (game_fetcher_tx, game_fetcher_rx) = GameFetcherRequest::channel();
    let (tx_manager_tx, tx_manager_rx) = TxManagerRequest::channel();

    // Fetch contract addresses
    let rollup_config = cl::get_rollup_config(&cl_rpc).await?;
    let (factory_address, registry_address) =
        el::get_factory_and_registry_addresses(&l1_rpc, rollup_config.l1_system_config_address)
            .await?;
    tracing::info!(?factory_address, ?registry_address, "Fetched contract addresses");

    // Create components
    let game_fetcher = GameFetcher::new(
        args.game_fetcher_config,
        state.clone(),
        l1_rpc.clone(),
        factory_address,
        registry_address,
    );

    let game_fetcher_broadcast_rx = game_fetcher.subscribe();

    let game_challenger = GameChallenger::new(
        args.game_challenger_config,
        state.clone(),
        l1_rpc.clone(),
        l2_rpc.clone(),
        game_fetcher_tx,
        tx_manager_tx,
    );

    let tx_manager = TxManager::new(args.tx_manager_config, l1_rpc.clone())?;

    // Start all the components
    let ct = CancellationToken::new();
    let tracker = TaskTracker::new();
    tracker.spawn(game_fetcher.start(ct.clone(), game_fetcher_rx));
    tracker.spawn(game_challenger.start(ct.clone(), game_fetcher_broadcast_rx));
    tracker.spawn(tx_manager.start(ct.clone(), tx_manager_rx));
    tracker.close();

    // Shutdown all the components
    signal::ctrl_c().await?;
    ct.cancel();
    tracker.wait().await;

    Ok(())
}
