use alloy_primitives::{BlockNumber, Bytes, B256, U256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_client::RpcClient;
use alloy_sol_types::SolValue;
use anyhow::Result;
use op_succinct_host_utils::DisputeGameFactory;
use rise_fp::{
    common::{
        args::{ClRpcArgs, L1RpcArgs, ProposerSignerArgs, SignerArgs},
        primitives::{GameIndex, RpcArgs},
    },
    rpc::{cl, el},
};

#[derive(clap::Args)]
pub struct ProposeCmd {
    #[command(flatten)]
    l1_rpc_args: L1RpcArgs,
    #[command(flatten)]
    cl_rpc_args: ClRpcArgs,
    #[command(flatten)]
    proposer_signer_args: ProposerSignerArgs,
    #[arg(long, default_value = "42")]
    game_type: u32,
    #[arg(long)]
    parent_game_index: Option<GameIndex>,
    #[arg(long)]
    claim_block: BlockNumber,
    #[arg(long)]
    output_root: Option<B256>,
}

impl ProposeCmd {
    pub async fn run(self) -> Result<()> {
        let l1_rpc: RpcClient = RpcArgs::from(self.l1_rpc_args).into();
        let cl_rpc: RpcClient = RpcArgs::from(self.cl_rpc_args).into();

        let rollup_config = cl::get_rollup_config(&cl_rpc).await?;
        let (factory_address, registry_address) =
            el::get_factory_and_registry_addresses(&l1_rpc, rollup_config.l1_system_config_address)
                .await?;
        tracing::info!(?factory_address, ?registry_address, "Fetched contract addresses");

        let l1_provider = ProviderBuilder::new().connect_client(l1_rpc.clone());
        let factory = DisputeGameFactory::new(factory_address, &l1_provider);

        let parent_game_index = if let Some(parent_game_index) = self.parent_game_index {
            tracing::info!(%parent_game_index, "Using the provided parent_game_index");
            parent_game_index
        } else {
            tracing::info!("Using the 0xffffffff as the default parent_game_index");
            GameIndex::MAX
        };

        let init_bond = factory.initBonds(self.game_type).call().await?;
        tracing::info!(%init_bond, "Fetched init bond");

        let extra_data =
            Bytes::from((U256::from(self.claim_block), parent_game_index).abi_encode_packed());
        tracing::info!(%extra_data, "Encoded extra data");

        let output_root = if let Some(output_root) = self.output_root {
            tracing::info!(%output_root, "Using the provided output_root");
            output_root
        } else {
            let resp = cl::get_output_at_block(&cl_rpc, self.claim_block).await?;
            tracing::info!(output_root = ?resp.output_root, "Fetched output_root");
            resp.output_root
        };

        let transaction_request = factory
            .create(self.game_type, output_root, extra_data)
            .value(init_bond)
            .into_transaction_request();
        tracing::info!(?transaction_request, "Broadcasting transaction...");

        let wallet = SignerArgs::from(self.proposer_signer_args.clone()).build_wallet()?;
        let l1_provider = ProviderBuilder::new().wallet(wallet).connect_client(l1_rpc.clone());

        let pending_tx = l1_provider.send_transaction(transaction_request).await?;
        tracing::info!(tx_hash = ?pending_tx.tx_hash(), "Awaiting confirmation...");

        let receipt = pending_tx.get_receipt().await?;
        tracing::info!(?receipt, "Submitted transaction successfully");
        Ok(())
    }
}
