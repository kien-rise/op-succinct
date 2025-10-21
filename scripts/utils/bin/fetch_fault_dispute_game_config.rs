use std::{env, sync::Arc};

use alloy_eips::BlockId;
use anyhow::Result;
use fault_proof::config::FaultDisputeGameConfig;
use op_succinct_host_utils::{
    fetcher::{OPSuccinctDataFetcher, RPCMode},
    host::OPSuccinctHost,
    OP_SUCCINCT_FAULT_DISPUTE_GAME_CONFIG_PATH,
};
use op_succinct_proof_utils::initialize_host;
use op_succinct_scripts::config_common::{
    find_project_root, get_shared_config_data, parse_addresses, write_config_file,
    TWO_WEEKS_IN_SECONDS,
};
use serde_json::Value;

/// Updates and generates the fault dispute game configuration file.
///
/// This function fetches the necessary configuration parameters from environment variables
/// and shared configuration data to generate a JSON configuration file used for deploying
/// the OPSuccinctFaultDisputeGame contract.
///
/// # Environment Variables
///
/// ## Game Configuration
/// - `GAME_TYPE`: Unique identifier for the dispute game type (default: "42")
///
/// ## Timing Configuration
/// - `DISPUTE_GAME_FINALITY_DELAY_SECONDS`: Delay in seconds before a dispute game can be finalized
///   (default: "604800" = 7 days)
/// - `MAX_CHALLENGE_DURATION`: Maximum duration in seconds for challenges (default: "604800" = 7
///   days)
/// - `MAX_PROVE_DURATION`: Maximum duration in seconds for proving (default: "86400" = 1 day)
/// - `FALLBACK_TIMEOUT_FP_SECS`: Timeout in seconds for permissionless proposing fallback (default:
///   1209600 = 2 weeks)
///
/// ## Bond Configuration
/// - `INITIAL_BOND_WEI`: Initial bond amount in wei required to create a dispute game (default:
///   "1000000000000000" = 0.001 ETH)
/// - `CHALLENGER_BOND_WEI`: Bond amount in wei required to challenge a game (default:
///   "1000000000000000" = 0.001 ETH)
///
/// ## Access Control Configuration
/// - `PERMISSIONLESS_MODE`: If "true", anyone can propose or challenge games; if "false", only
///   authorized addresses can (default: "false")
/// - `PROPOSER_ADDRESSES`: Comma-separated list of addresses authorized to propose games (ignored
///   if permissionless mode is true)
/// - `CHALLENGER_ADDRESSES`: Comma-separated list of addresses authorized to challenge games
///   (ignored if permissionless mode is true)
///
/// ## Contract Configuration
/// - `OPTIMISM_PORTAL2_ADDRESS`: Address of the OptimismPortal2 contract. If not provided or set to
///   zero address, a MockOptimismPortal2 will be deployed (default: zero address)
///
/// ## Starting State Configuration
/// - `STARTING_L2_BLOCK_NUMBER`: L2 block number to use as the starting point for the dispute game.
///   If not provided, it's calculated as: `latest_finalized_block -
///   (dispute_game_finality_delay_seconds / block_time)`
///
/// # Shared Configuration
///
/// The function also retrieves the following from shared configuration data:
/// - `aggregation_vkey`: Aggregation verification key
/// - `range_vkey_commitment`: Range verification key commitment
/// - `rollup_config_hash`: Hash of the rollup configuration
/// - `verifier_address`: Address of the SP1 verifier contract
/// - `use_sp1_mock_verifier`: Whether to use the mock verifier for testing
///
/// # Output
///
/// Generates `contracts/opsuccinctfdgconfig.json` containing all configuration parameters
/// needed for the Solidity deployment scripts.
async fn update_fdg_config() -> Result<()> {
    log::info!("Starting fault dispute game configuration update");

    log::info!("Initializing data fetcher with rollup config");
    let data_fetcher = OPSuccinctDataFetcher::new_with_rollup_config().await?;
    log::info!("Data fetcher initialized successfully");

    log::info!("Initializing host");
    let host = initialize_host(Arc::new(data_fetcher.clone()));
    log::info!("Host initialized successfully");

    log::info!("Fetching shared configuration data");
    let shared_config = get_shared_config_data().await?;
    log::info!("Shared configuration loaded successfully");

    // Game configuration.
    log::info!("Reading game configuration");
    let game_type_str = env::var("GAME_TYPE").unwrap_or("42".to_string());
    log::info!("GAME_TYPE: {}", game_type_str);
    let game_type = game_type_str.parse().unwrap();
    log::info!("Game type set to: {}", game_type);

    // Timing configuration.
    log::info!("Reading timing configuration");
    let dispute_game_finality_delay_str =
        env::var("DISPUTE_GAME_FINALITY_DELAY_SECONDS").unwrap_or("604800".to_string()); // 7 days default
    log::info!("DISPUTE_GAME_FINALITY_DELAY_SECONDS: {}", dispute_game_finality_delay_str);
    let dispute_game_finality_delay_seconds = dispute_game_finality_delay_str.parse().unwrap();
    log::info!("Dispute game finality delay: {} seconds", dispute_game_finality_delay_seconds);

    let max_challenge_duration_str =
        env::var("MAX_CHALLENGE_DURATION").unwrap_or("604800".to_string()); // 7 days default
    log::info!("MAX_CHALLENGE_DURATION: {}", max_challenge_duration_str);
    let max_challenge_duration = max_challenge_duration_str.parse().unwrap();
    log::info!("Max challenge duration: {} seconds", max_challenge_duration);

    let max_prove_duration_str = env::var("MAX_PROVE_DURATION").unwrap_or("86400".to_string()); // 1 day default
    log::info!("MAX_PROVE_DURATION: {}", max_prove_duration_str);
    let max_prove_duration = max_prove_duration_str.parse().unwrap();
    log::info!("Max prove duration: {} seconds", max_prove_duration);

    let fallback_timeout_fp_secs = env::var("FALLBACK_TIMEOUT_FP_SECS")
        .map(|p| {
            log::info!("FALLBACK_TIMEOUT_FP_SECS: {}", p);
            p.parse().unwrap()
        })
        .unwrap_or_else(|_| {
            log::info!("FALLBACK_TIMEOUT_FP_SECS not set, using default: {}", TWO_WEEKS_IN_SECONDS);
            TWO_WEEKS_IN_SECONDS
        });
    log::info!("Fallback timeout: {} seconds", fallback_timeout_fp_secs);

    // Bond configuration.
    log::info!("Reading bond configuration");
    let initial_bond_wei_str =
        env::var("INITIAL_BOND_WEI").unwrap_or("1000000000000000".to_string()); // 0.001 ETH default
    log::info!("INITIAL_BOND_WEI: {}", initial_bond_wei_str);
    let initial_bond_wei = initial_bond_wei_str.parse().unwrap();
    log::info!("Initial bond: {} wei", initial_bond_wei);

    let challenger_bond_wei_str =
        env::var("CHALLENGER_BOND_WEI").unwrap_or("1000000000000000".to_string()); // 0.001 ETH default
    log::info!("CHALLENGER_BOND_WEI: {}", challenger_bond_wei_str);
    let challenger_bond_wei = challenger_bond_wei_str.parse().unwrap();
    log::info!("Challenger bond: {} wei", challenger_bond_wei);

    // Access control configuration.
    log::info!("Reading access control configuration");
    let permissionless_mode_str = env::var("PERMISSIONLESS_MODE").unwrap_or("false".to_string());
    log::info!("PERMISSIONLESS_MODE: {}", permissionless_mode_str);
    let permissionless_mode = permissionless_mode_str.parse().unwrap();
    log::info!("Permissionless mode: {}", permissionless_mode);

    let proposer_addresses = if permissionless_mode {
        log::info!("Permissionless mode enabled - no proposer address restrictions");
        vec![]
    } else {
        log::info!("Parsing proposer addresses from PROPOSER_ADDRESSES");
        let addresses = parse_addresses("PROPOSER_ADDRESSES");
        log::info!("Proposer addresses configured: {:?}", addresses);
        addresses
    };

    let challenger_addresses = if permissionless_mode {
        log::info!("Permissionless mode enabled - no challenger address restrictions");
        vec![]
    } else {
        log::info!("Parsing challenger addresses from CHALLENGER_ADDRESSES");
        let addresses = parse_addresses("CHALLENGER_ADDRESSES");
        log::info!("Challenger addresses configured: {:?}", addresses);
        addresses
    };

    // OptimismPortal2 configuration.
    log::info!("Reading OptimismPortal2 configuration");
    let optimism_portal2_address = env::var("OPTIMISM_PORTAL2_ADDRESS").unwrap_or_else(|_| {
        log::info!("OPTIMISM_PORTAL2_ADDRESS not set, using zero address (MockOptimismPortal2 will be deployed)");
        "0x0000000000000000000000000000000000000000".to_string()
    });
    log::info!("OptimismPortal2 address: {}", optimism_portal2_address);

    // Get starting block number - use `latest finalized - dispute game finality delay` if not set.
    log::info!("Determining starting L2 block number");
    let starting_l2_block_number = match env::var("STARTING_L2_BLOCK_NUMBER") {
        Ok(n) => {
            log::info!("STARTING_L2_BLOCK_NUMBER provided: {}", n);
            let block_num = n.parse().unwrap();
            log::info!("Using provided starting L2 block number: {}", block_num);
            block_num
        }
        Err(_) => {
            log::info!("STARTING_L2_BLOCK_NUMBER not provided, calculating automatically");
            // Use finalized block minus the finality delay as a starting point
            log::info!("Fetching finalized L2 header");
            let finalized_l2_header = data_fetcher.get_l2_header(BlockId::finalized()).await?;
            let finalized_l2_block = finalized_l2_header.number;
            log::info!("Current finalized L2 block: {}", finalized_l2_block);

            log::info!("Getting block time from rollup config");
            let block_time = &data_fetcher
                .rollup_config
                .as_ref()
                .ok_or(anyhow::anyhow!("Rollup config not found"))?
                .block_time;
            log::info!("Block time: {} seconds", block_time);

            let num_blocks_for_finality = dispute_game_finality_delay_seconds / block_time;
            log::info!("Number of blocks for finality delay: {}", num_blocks_for_finality);
            let search_start = finalized_l2_block.saturating_sub(num_blocks_for_finality);
            log::info!("Search start block: {}", search_start);

            // Now search for the highest finalized block with available data
            log::info!(
                "Searching for highest finalized block with available data from block {}",
                search_start
            );
            let finalized_l2_block_number =
                match host.get_finalized_l2_block_number(&data_fetcher, search_start).await? {
                    Some(block_num) => {
                        log::info!("Found finalized block with data: {}", block_num);
                        block_num
                    }
                    None => {
                        log::warn!(
                            "No finalized block with data found, using search start: {}",
                            search_start
                        );
                        search_start
                    }
                };

            let calculated_start =
                finalized_l2_block_number.saturating_sub(num_blocks_for_finality);
            log::info!("Calculated starting L2 block number: {}", calculated_start);
            calculated_start
        }
    };

    let starting_block_number_hex = format!("0x{starting_l2_block_number:x}");
    log::info!(
        "Fetching output data for starting block {} ({})",
        starting_l2_block_number,
        starting_block_number_hex
    );

    log::info!("Calling optimism_outputAtBlock RPC method");
    let optimism_output_data: Value = data_fetcher
        .fetch_rpc_data_with_mode(
            RPCMode::L2Node,
            "optimism_outputAtBlock",
            vec![starting_block_number_hex.into()],
        )
        .await?;

    log::info!("Output data response: {:?}", optimism_output_data);

    let starting_output_root = optimism_output_data["outputRoot"].as_str().unwrap().to_string();
    log::info!("Starting output root: {}", starting_output_root);

    log::info!("Creating fault dispute game configuration");
    let fdg_config = FaultDisputeGameConfig {
        aggregation_vkey: shared_config.aggregation_vkey,
        challenger_addresses,
        challenger_bond_wei,
        dispute_game_finality_delay_seconds,
        fallback_timeout_fp_secs,
        game_type,
        initial_bond_wei,
        max_challenge_duration,
        max_prove_duration,
        optimism_portal2_address,
        permissionless_mode,
        proposer_addresses,
        range_vkey_commitment: shared_config.range_vkey_commitment,
        rollup_config_hash: shared_config.rollup_config_hash,
        starting_l2_block_number,
        starting_root: starting_output_root,
        use_sp1_mock_verifier: shared_config.use_sp1_mock_verifier,
        verifier_address: shared_config.verifier_address,
    };

    log::info!("Writing configuration to file");
    write_config_file(
        &fdg_config,
        &OP_SUCCINCT_FAULT_DISPUTE_GAME_CONFIG_PATH,
        "Fault Dispute Game",
    )?;

    log::info!("Fault dispute game configuration updated successfully");

    Ok(())
}

use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Environment file to load
    #[arg(long, default_value = ".env")]
    env_file: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    sp1_sdk::utils::setup_logger();

    let args = Args::parse();
    log::info!("Starting fetch_fault_dispute_game_config with env file: {}", args.env_file);

    // This fetches the .env file from the project root. If the command is invoked in the contracts/
    // directory, the .env file in the root of the repo is used.
    if let Some(root) = find_project_root() {
        let env_path = root.join(&args.env_file);
        log::info!("Loading environment file from: {:?}", env_path);
        dotenv::from_path(&env_path).ok();
        log::info!("Environment file loaded successfully");
    } else {
        log::warn!("Could not find project root. {} file not loaded.", args.env_file);
    }

    update_fdg_config().await?;
    log::info!("Program completed successfully");

    Ok(())
}
