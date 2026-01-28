use std::{collections::BTreeMap, time::Duration};

use alloy_consensus::Header;
use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_network::{AnyRpcBlock, Ethereum, Network, NetworkWallet};
use alloy_primitives::{Address, BlockNumber, Bytes, B256, U256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rlp::Decodable;
use alloy_rpc_client::RpcClient;
use alloy_sol_types::{SolEvent, SolValue};
use alloy_transport::{RpcError, TransportResult};
use anyhow::{anyhow, bail, Result};
use kona_rpc::{OutputResponse, SafeHeadResponse};

use crate::contract::{
    AnchorStateRegistry,
    DisputeGameFactory::{self, DisputeGameCreated},
    IDisputeGame,
};

pub type GameIndex = u32;
pub type GameType = u32;

#[allow(non_snake_case)]
pub async fn debug_getRawHeader(el_rpc: &RpcClient, block_id: BlockId) -> TransportResult<Header> {
    let raw_header: Bytes = el_rpc.request("debug_getRawHeader", (block_id,)).await?;
    let mut data = raw_header.as_ref();
    let header = Header::decode(&mut data).map_err(RpcError::local_usage)?;
    Ok(header)
}

#[allow(non_snake_case)]
pub async fn optimism_safeHeadAtL1Block(
    cl_rpc: &RpcClient,
    block_number: BlockNumber,
) -> TransportResult<SafeHeadResponse> {
    cl_rpc.request("optimism_safeHeadAtL1Block", (BlockNumberOrTag::Number(block_number),)).await
}

#[allow(non_snake_case)]
pub async fn optimism_outputAtBlock(
    cl_rpc: &RpcClient,
    block_number: BlockNumber,
) -> TransportResult<OutputResponse> {
    cl_rpc.request("optimism_outputAtBlock", (BlockNumberOrTag::Number(block_number),)).await
}

#[allow(non_snake_case)]
pub async fn eth_getBlockByNumber(
    el_rpc: &RpcClient,
    block_number: BlockNumberOrTag,
    full: bool,
) -> TransportResult<Option<AnyRpcBlock>> {
    el_rpc.request("eth_getBlockByNumber", (block_number, full)).await
}

async fn get_finalized_block_number(el_rpc: &RpcClient) -> Result<BlockNumber> {
    if let Ok(header) = debug_getRawHeader(el_rpc, BlockId::finalized()).await {
        return Ok(header.number);
    }
    match eth_getBlockByNumber(el_rpc, BlockNumberOrTag::Finalized, false).await? {
        Some(block) => Ok(block.header.number),
        None => Err(anyhow!("failed to get finalized head")),
    }
}

pub async fn get_withdrawals_root(
    el_rpc: &RpcClient,
    block_number: BlockNumberOrTag,
) -> Result<B256> {
    let withdrawals_root = match debug_getRawHeader(el_rpc, block_number.into()).await {
        Ok(header) => header.withdrawals_root,
        Err(_) => eth_getBlockByNumber(el_rpc, block_number, false)
            .await?
            .and_then(|block| block.header.withdrawals_root),
    };

    withdrawals_root.ok_or_else(|| anyhow::anyhow!("withdrawals_root not found in block"))
}

async fn get_safe_l1_block_for_l2_block(
    cl_rpc: &RpcClient,
    l1_finalized_block_number: BlockNumber,
    l2_block_number: BlockNumber,
) -> Result<Option<BlockNumber>> {
    tracing::debug!(
        l1_finalized_block_number,
        l2_block_number,
        "get_safe_l1_block_for_l2_block: start"
    );

    let l1_origin = optimism_outputAtBlock(cl_rpc, l2_block_number).await?.block_ref.l1_origin;

    let mut lo = l1_origin.number;
    let mut hi = l1_finalized_block_number + 1;

    while lo < hi {
        let md = lo + (hi - lo) / 2;
        let l2_safe_block_number = optimism_safeHeadAtL1Block(cl_rpc, md).await?.safe_head.number;
        if l2_safe_block_number >= l2_block_number {
            hi = md
        } else {
            lo = md + 1
        }
    }

    let safe_l1_block_number = if lo <= l1_finalized_block_number { Some(lo) } else { None };

    tracing::debug!(?safe_l1_block_number, "get_safe_l1_block_for_l2_block: end");
    Ok(safe_l1_block_number)
}

async fn get_l2_block_number_at_game_index(
    l1_rpc: &RpcClient,
    anchor_state_registry_address: Address,
    dispute_game_factory_address: Address,
    game_index: GameIndex,
) -> Result<BlockNumber> {
    tracing::debug!(game_index, "get_l2_block_number_at_game_index: start");

    let l1_provider = ProviderBuilder::new().connect_client(l1_rpc.clone());

    let l2_block_number: U256 = if game_index == GameIndex::MAX {
        let anchor_state_registry =
            AnchorStateRegistry::new(anchor_state_registry_address, l1_provider);
        anchor_state_registry.getAnchorRoot().call().await?._1
    } else {
        let dispute_game_factory =
            DisputeGameFactory::new(dispute_game_factory_address, l1_provider.clone());
        let game_address =
            dispute_game_factory.gameAtIndex(U256::from(game_index)).call().await?.proxy;
        let game = IDisputeGame::new(game_address, l1_provider.clone());
        game.l2SequenceNumber().call().await?
    };

    tracing::debug!(?l2_block_number, "get_l2_block_number_at_game_index: end");
    Ok(l2_block_number.to())
}

pub async fn get_next_games_to_create(
    l1_rpc: &RpcClient,
    l2_rpc: &RpcClient,
    cl_rpc: &RpcClient,
    anchor_state_registry_address: Address,
    dispute_game_factory_address: Address,
    starting_game_index: GameIndex,
    proposal_interval_in_blocks: u64,
    limit: usize,
) -> Result<Vec<BlockNumber>> {
    tracing::debug!(
        starting_game_index,
        proposal_interval_in_blocks,
        ?limit,
        "get_next_games_to_create: start"
    );

    // TODO: check respected game type

    let starting_l2_block_number = get_l2_block_number_at_game_index(
        l1_rpc,
        anchor_state_registry_address,
        dispute_game_factory_address,
        starting_game_index,
    )
    .await?;

    let l1_finalized_block_number = get_finalized_block_number(l1_rpc).await?;
    let l2_finalized_block_number = get_finalized_block_number(l2_rpc).await?;

    if starting_l2_block_number > l2_finalized_block_number {
        bail!("starting L2 block is ahead of the finalized L2 block");
    }

    let mut current_l2_block_number = starting_l2_block_number;
    let mut l2_block_numbers = Vec::new();

    while l2_block_numbers.len() < limit {
        let next_l2_block_number = if proposal_interval_in_blocks > 0 {
            current_l2_block_number + proposal_interval_in_blocks
        } else if let Some(l1_block_number) = get_safe_l1_block_for_l2_block(
            cl_rpc,
            l1_finalized_block_number,
            current_l2_block_number + 1,
        )
        .await?
        {
            let l2_safe_head = optimism_safeHeadAtL1Block(cl_rpc, l1_block_number).await?;
            l2_safe_head.safe_head.number
        } else {
            BlockNumber::MAX
        };

        if next_l2_block_number <= current_l2_block_number {
            bail!("next L2 block number must strictly increase from the current block");
        } else if next_l2_block_number > l2_finalized_block_number {
            break;
        }

        tracing::debug!(next_l2_block_number, "get_next_games_to_create: append");
        l2_block_numbers.push(next_l2_block_number);
        current_l2_block_number = next_l2_block_number;
    }

    tracing::debug!(?l2_block_numbers, "get_next_games_to_create: end");
    Ok(l2_block_numbers)
}

async fn get_game_index<P: Provider<N>, N: Network>(
    dispute_game_factory: &DisputeGameFactory::DisputeGameFactoryInstance<P, N>,
    game_address: Address,
    max_distance: u32,
) -> Result<GameIndex> {
    let game_count: GameIndex = dispute_game_factory.gameCount().call().await?.to();
    for i in 1..=game_count {
        if i > max_distance {
            bail!("max attempts reached without finding the game")
        }
        let game_index = game_count - i;
        let returned = dispute_game_factory.gameAtIndex(U256::from(game_index)).call().await?;
        if returned.proxy == game_address {
            return Ok(game_index)
        }
    }
    bail!("cannot find game with address: {:?}", game_address)
}

async fn create_game(
    l1_rpc: &RpcClient,
    dispute_game_factory_address: Address,
    signer: impl NetworkWallet<Ethereum> + Clone,
    game_type: GameType,
    output_root: B256,
    extra_data: Bytes,
    init_bond: U256,
) -> Result<(GameIndex, Address)> {
    let l1_provider = ProviderBuilder::new().wallet(signer).connect_client(l1_rpc.clone());
    let dispute_game_factory =
        DisputeGameFactory::new(dispute_game_factory_address, l1_provider.clone());

    let transaction_request = dispute_game_factory
        .create(game_type, output_root, extra_data)
        .value(init_bond)
        .into_transaction_request();

    const NUM_CONFIRMATIONS: u64 = 1;
    const TIMEOUT_SECONDS: u64 = 60;

    let receipt = l1_provider
        .send_transaction(transaction_request)
        .await?
        .with_required_confirmations(NUM_CONFIRMATIONS)
        .with_timeout(Some(Duration::from_secs(TIMEOUT_SECONDS)))
        .get_receipt()
        .await?;
    if !receipt.status() {
        bail!("tx reverted: {:?}", receipt);
    };

    let game_address = match (receipt.inner.logs().iter())
        .find(|log| log.topic0() == Some(&DisputeGameCreated::SIGNATURE_HASH))
    {
        Some(log) => DisputeGameCreated::decode_log(&log.inner)?.disputeProxy,
        None => bail!("cannot find DisputeGameCreated event"),
    };

    const MAX_DISTANCE: u32 = 16;
    let game_index = get_game_index(&dispute_game_factory, game_address, MAX_DISTANCE).await?;

    Ok((game_index, game_address))
}

pub async fn create_games(
    l1_rpc: &RpcClient,
    cl_rpc: &RpcClient,
    signer: impl NetworkWallet<Ethereum> + Clone,
    dispute_game_factory_address: Address,
    game_type: GameType,
    init_bond: U256,
    starting_game_index: GameIndex,
    l2_block_numbers: &[BlockNumber],
) -> Result<BTreeMap<GameIndex, Address>> {
    tracing::debug!(?init_bond, starting_game_index, ?l2_block_numbers, "create_games: start");

    let l1_provider = ProviderBuilder::new().connect_client(l1_rpc.clone());
    let dispute_game_factory =
        DisputeGameFactory::new(dispute_game_factory_address, l1_provider.clone());

    let mut parent_game_index = starting_game_index;
    let mut created_games = BTreeMap::new();

    for bn in l2_block_numbers.iter().copied() {
        let output_root = optimism_outputAtBlock(cl_rpc, bn).await?.output_root;
        let extra_data = Bytes::from((U256::from(bn), parent_game_index).abi_encode_packed());

        tracing::debug!(game_type, ?output_root, ?extra_data, "create_games: creating game");

        let existing_game_address = dispute_game_factory
            .games(game_type, output_root, extra_data.clone())
            .call()
            .await?
            .proxy;
        if !existing_game_address.is_zero() {
            const MAX_DISTANCE: u32 = 16;
            if let Ok(game_index) =
                get_game_index(&dispute_game_factory, existing_game_address, MAX_DISTANCE).await
            {
                tracing::warn!(game_index, ?existing_game_address, "game already exists");
            } else {
                tracing::warn!("game already exists");
            }
            // TODO: If game cannot be eventually valid,
            // then we should skip then continue.
            // See check_game_claim_eventual_validity.
            break;
        }

        match create_game(
            &l1_rpc,
            dispute_game_factory_address,
            signer.clone(),
            game_type,
            output_root,
            extra_data,
            init_bond,
        )
        .await
        {
            Ok((game_index, game_address)) => {
                tracing::debug!(game_index, ?game_address, "create_games: game created");
                created_games.insert(game_index, game_address);
                parent_game_index = game_index;
            }
            Err(err) => {
                if created_games.is_empty() {
                    // no progress is made, we can directly throw an error here
                    return Err(err);
                } else {
                    // some progress has been made, we will return partial result
                    tracing::warn!("create_games: create game failed: {:?}", err);
                    break;
                }
            }
        }
    }

    Ok(created_games)
}

pub async fn get_init_bond(
    l1_rpc: &RpcClient,
    dispute_game_factory_address: Address,
    game_type: GameType,
) -> Result<U256> {
    let l1_provider = ProviderBuilder::new().connect_client(l1_rpc.clone());
    let dispute_game_factory =
        DisputeGameFactory::new(dispute_game_factory_address, l1_provider.clone());
    let init_bond = dispute_game_factory.initBonds(game_type).call().await?;
    Ok(init_bond)
}

#[cfg(all(test, feature = "integration"))]
mod tests {
    use super::*;

    use std::usize;

    use alloy_rpc_client::RpcClient;
    use alloy_signer_local::PrivateKeySigner;
    use alloy_transport_http::reqwest::Url;
    use clap::Parser;
    use op_succinct_signer_utils::Signer;

    /*
        cargo test --release --package op-succinct-fp --lib \
            --features integration --features eigenda -- \
            rise::tests::test_debug_get_raw_header --exact --nocapture --ignored -- \
            "$RPC_URL" "$BLOCK_ID"
    */
    #[tokio::test]
    #[ignore]
    async fn test_debug_get_raw_header() -> anyhow::Result<()> {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_target(true)
            .init();

        #[derive(Parser, Debug)]
        struct Args {
            rpc_url: Url,
            block_id: BlockId,
        }

        let args = Args::parse_from(std::env::args_os().skip_while(|arg| arg != "--"));
        let rpc_client = RpcClient::new_http(args.rpc_url);
        let header = debug_getRawHeader(&rpc_client, args.block_id).await?;
        println!("{:#?}", header);
        Ok(())
    }

    /*
        cargo test --release --package op-succinct-fp --lib \
            --features integration --features eigenda -- \
            rise::tests::test_get_next_games_to_create --exact --nocapture --ignored -- \
            --l1-rpc-url "$L1_RPC_URL" \
            --l2-rpc-url "$L2_RPC_URL" \
            --cl-rpc-url "$CL_RPC_URL" \
            --anchor-state-registry-address "$ANCHOR_STATE_REGISTRY_ADDRESS" \
            --dispute-game-factory-address "$DISPUTE_GAME_FACTORY_ADDRESS" \
            --starting-game-index "$STARTING_GAME_INDEX" \
            --proposal-interval-in-blocks "$PROPOSAL_INTERVAL_IN_BLOCKS" \
            --limit "$LIMIT"
    */
    #[tokio::test]
    #[ignore]
    async fn test_get_next_games_to_create() -> anyhow::Result<()> {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_target(true)
            .init();

        #[derive(Parser, Debug)]
        struct Args {
            #[arg(long)]
            l1_rpc_url: Url,
            #[arg(long)]
            l2_rpc_url: Url,
            #[arg(long)]
            cl_rpc_url: Url,
            #[arg(long)]
            anchor_state_registry_address: Address,
            #[arg(long)]
            dispute_game_factory_address: Address,
            #[arg(long)]
            starting_game_index: GameIndex,
            #[arg(long)]
            proposal_interval_in_blocks: u64,
            #[arg(long)]
            limit: Option<usize>,
        }

        let args = Args::parse_from(std::env::args_os().skip_while(|arg| arg != "--"));
        let l1_rpc = RpcClient::new_http(args.l1_rpc_url);
        let l2_rpc = RpcClient::new_http(args.l2_rpc_url);
        let cl_rpc = RpcClient::new_http(args.cl_rpc_url);

        let l2_block_numbers = get_next_games_to_create(
            &l1_rpc,
            &l2_rpc,
            &cl_rpc,
            args.anchor_state_registry_address,
            args.dispute_game_factory_address,
            args.starting_game_index,
            args.proposal_interval_in_blocks,
            args.limit.unwrap_or(usize::MAX),
        )
        .await?;

        println!("{:#?}", l2_block_numbers);
        Ok(())
    }

    /*
        cargo test --release --package op-succinct-fp --lib \
            --features integration --features eigenda -- \
            rise::tests::test_create_games --exact --nocapture --ignored -- \
            --l1-rpc-url "$L1_RPC_URL" \
            --cl-rpc-url "$CL_RPC_URL" \
            --anchor-state-registry-address "$ANCHOR_STATE_REGISTRY_ADDRESS" \
            --dispute-game-factory-address "$DISPUTE_GAME_FACTORY_ADDRESS" \
            --starting-game-index "$STARTING_GAME_INDEX" \
            --l2-block-numbers "$L2_BLOCK_NUMBERS" \
            --private-key "$PRIVATE_KEY"
    */
    #[tokio::test]
    #[ignore]
    async fn test_create_games() -> anyhow::Result<()> {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_target(true)
            .init();

        #[derive(Parser, Debug)]
        struct Args {
            #[arg(long)]
            l1_rpc_url: Url,
            #[arg(long)]
            cl_rpc_url: Url,
            #[arg(long)]
            anchor_state_registry_address: Address,
            #[arg(long)]
            dispute_game_factory_address: Address,
            #[arg(long)]
            starting_game_index: GameIndex,
            #[arg(long, value_delimiter = ',')]
            l2_block_numbers: Vec<BlockNumber>,
            #[arg(long)]
            private_key: B256,
        }

        let args = Args::parse_from(std::env::args_os().skip_while(|arg| arg != "--"));
        let l1_rpc = RpcClient::new_http(args.l1_rpc_url);
        let cl_rpc = RpcClient::new_http(args.cl_rpc_url);

        const GAME_TYPE: u32 = 42;
        let init_bond =
            get_init_bond(&l1_rpc, args.dispute_game_factory_address, GAME_TYPE).await?;

        let signer = Signer::LocalSigner(PrivateKeySigner::from_bytes(&args.private_key)?);

        let games = create_games(
            &l1_rpc,
            &cl_rpc,
            &signer,
            args.dispute_game_factory_address,
            GAME_TYPE,
            init_bond,
            args.starting_game_index,
            &args.l2_block_numbers,
        )
        .await?;

        println!("{:#?}", games);
        Ok(())
    }
}
