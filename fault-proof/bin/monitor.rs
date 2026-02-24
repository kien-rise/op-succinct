use std::{collections::BTreeMap, time::Duration};

use alloy_primitives::{Address, BlockNumber, B256, U256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_client::RpcClient;
use alloy_transport_http::reqwest::Url;
use anyhow::{bail, Result};
use clap::Parser;
use fault_proof::contract::{
    BondDistributionMode, ClaimData, DisputeGameFactory, GameStatus, OPSuccinctFaultDisputeGame,
};
use op_succinct_host_utils::fetcher::get_rpc_client;
use tracing_subscriber::EnvFilter;

type GameIndex = u32;

const GAME_TYPE: u32 = 42;

#[derive(Debug, Clone)]
struct Game {
    // TODO: check the mutable fields
    address: Address,
    starting_block_number: BlockNumber,
    l2_block_number: BlockNumber,
    root_claim: B256,
    was_respected_game_type_when_created: bool,
    status: GameStatus,
    claim_data: ClaimData,
    challenger_bond: U256,
    bond_distribution_mode: BondDistributionMode,
}

#[derive(Debug, Clone, Default)]
struct State {
    games: BTreeMap<GameIndex, Game>,
}

impl State {
    async fn fetch_game<P: Provider + Clone>(
        &mut self,
        game_index: GameIndex,
        l1_provider: P,
        dispute_game_factory_address: Address,
    ) -> Result<Game> {
        let dispute_game_factory =
            DisputeGameFactory::new(dispute_game_factory_address, l1_provider.clone());

        let game_at_index = dispute_game_factory.gameAtIndex(U256::from(game_index)).call().await?;

        if game_at_index.gameType != GAME_TYPE {
            bail!("unsupported game type: {}", game_at_index.gameType);
        }

        let game_address = game_at_index.proxy;

        let contract = OPSuccinctFaultDisputeGame::new(game_address, l1_provider.clone());

        let multicall = l1_provider
            .multicall()
            .add(contract.startingBlockNumber())
            .add(contract.l2BlockNumber())
            .add(contract.rootClaim())
            .add(contract.wasRespectedGameTypeWhenCreated())
            .add(contract.status())
            .add(contract.claimData())
            .add(contract.challengerBond())
            .add(contract.bondDistributionMode());

        let (
            starting_block_number,
            l2_block_number,
            root_claim,
            was_respected_game_type_when_created,
            status,
            claim_data,
            challenger_bond,
            bond_distribution_mode,
        ) = multicall.aggregate().await?;

        Ok(Game {
            address: game_address,
            starting_block_number: starting_block_number.to(),
            l2_block_number: l2_block_number.to(),
            root_claim,
            was_respected_game_type_when_created,
            status,
            claim_data,
            challenger_bond,
            bond_distribution_mode,
        })
    }

    async fn step_sync(
        &mut self,
        l1_rpc_client: RpcClient,
        dispute_game_factory_address: Address,
        game_count: Option<GameIndex>,
    ) -> Result<bool> {
        let l1_provider = ProviderBuilder::new().connect_client(l1_rpc_client);
        let dispute_game_factory =
            DisputeGameFactory::new(dispute_game_factory_address, l1_provider.clone());
        let game_count = match game_count {
            Some(game_count) => game_count,
            None => dispute_game_factory.gameCount().call().await?.to(),
        };
        let first_unsynced_game_index = {
            (1..=game_count)
                .map(|i| game_count - i)
                .find(|game_index| !self.games.contains_key(game_index))
        };

        tracing::debug!(game_count, first_unsynced_game_index, "step_sync");

        if let Some(game_index) = first_unsynced_game_index {
            let game = self
                .fetch_game(game_index, l1_provider.clone(), dispute_game_factory_address)
                .await?;
            tracing::info!(game_index, game = ?game, "Game added");
            self.games.insert(game_index, game);
        };

        let progressed = first_unsynced_game_index.is_some();
        Ok(progressed)
    }
}

#[global_allocator]
static ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[derive(Debug, clap::Parser)]
struct Args {
    #[arg(long)]
    l1_rpc: Url,
    #[arg(long)]
    l1_requests_per_second: Option<f64>,
    #[arg(long)]
    dispute_game_factory_address: Address,
    #[arg(long)]
    game_count: Option<GameIndex>,
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

    let l1_rpc_client = get_rpc_client(args.l1_rpc.clone(), args.l1_requests_per_second);

    let mut state = State::default();

    loop {
        let progressed = state
            .step_sync(l1_rpc_client.clone(), args.dispute_game_factory_address, args.game_count)
            .await?;
        if !progressed {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    }
}
