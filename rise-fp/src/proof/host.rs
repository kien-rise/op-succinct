use std::{collections::HashMap, sync::Arc};

use alloy_primitives::{BlockNumber, ChainId, FixedBytes, B256, U256};
use alloy_rlp::Decodable;
use alloy_rpc_client::RpcClient;
use anyhow::Result;
use async_trait::async_trait;
use canoe_provider::CanoeInput;
use canoe_verifier_address_fetcher::CanoeVerifierAddressFetcherDeployedByEigenLabs;
use hokulea_host_bin::{cfg::SingleChainProvidersWithEigenDA, handler::fetch_eigenda_hint};
use hokulea_proof::{hint::ExtendedHintType, EigenDAPreimage};
use hokulea_witgen::from_eigenda_preimage_to_canoe_inputs;
use kona_genesis::{L1ChainConfig, RollupConfig};
use kona_host::{
    single::{SingleChainHintHandler, SingleChainHost},
    HintHandler, MemoryKeyValueStore, OnlineHostBackend, OnlineHostBackendCfg, PreimageServer,
    SharedKeyValueStore,
};
use kona_preimage::{
    BidirectionalChannel, HintReader, OracleServer, PreimageKey, PreimageOracleClient,
};
use kona_proof::{boot, BootInfo, Hint};
use op_succinct_client_utils::witness::{
    preimage_store::PreimageStore, BlobData, EigenDAWitnessData,
};
use op_succinct_eigenda_host_utils::witness_generator::EigenDAWitnessGenerator;
use op_succinct_host_utils::witness_generation::traits::WitnessGenerator;
use sp1_sdk::{SP1Proof, SP1Stdin};

// https://github.com/op-rs/kona/blob/bfcb26d8c76224a1a62f2ded67434ace9ed59a7e/bin/host/src/single/cfg.rs#L58
pub struct RiseCfg {
    pub l1_head: B256,
    pub agreed_l2_head_hash: B256,
    pub agreed_l2_output_root: B256,
    pub claimed_l2_output_root: B256,
    pub claimed_l2_block_number: BlockNumber,
    pub l2_chain_id: ChainId,
    pub rollup_config: Arc<RollupConfig>,
    pub l1_config: Arc<L1ChainConfig>,
}

impl OnlineHostBackendCfg for RiseCfg {
    type HintType = ExtendedHintType;
    type Providers = SingleChainProvidersWithEigenDA;
}

impl RiseCfg {
    // https://github.com/op-rs/kona/blob/24ff29c8a7caa3a33bd40a5639a421f785011471/bin/host/src/single/local_kv.rs#L28
    pub fn get_local_key_value_store(&self) -> MemoryKeyValueStore {
        let mut store = HashMap::<B256, Vec<u8>>::new();
        let encode = |local_key: U256| PreimageKey::new_local(local_key.to()).into();
        store.insert(
            encode(boot::L1_HEAD_KEY), //
            self.l1_head.to_vec(),
        );
        store.insert(
            encode(boot::L2_OUTPUT_ROOT_KEY), //
            self.agreed_l2_output_root.to_vec(),
        );
        store.insert(
            encode(boot::L2_CLAIM_KEY), //
            self.claimed_l2_output_root.to_vec(),
        );
        store.insert(
            encode(boot::L2_CLAIM_BLOCK_NUMBER_KEY), //
            self.claimed_l2_block_number.to_be_bytes().to_vec(),
        );
        store.insert(
            encode(boot::L2_CHAIN_ID_KEY), //
            self.l2_chain_id.to_be_bytes().to_vec(),
        );
        store.insert(
            encode(boot::L2_ROLLUP_CONFIG_KEY), //
            serde_json::to_vec(&self.rollup_config).unwrap(),
        );
        store.insert(
            encode(boot::L1_CONFIG_KEY), //
            serde_json::to_vec(&self.l1_config).unwrap(),
        );
        MemoryKeyValueStore { store }
    }

    fn get_legacy_cfg(&self) -> SingleChainHost {
        SingleChainHost {
            l1_head: self.l1_head,
            agreed_l2_head_hash: self.agreed_l2_head_hash,
            agreed_l2_output_root: self.agreed_l2_output_root,
            claimed_l2_output_root: self.claimed_l2_output_root,
            claimed_l2_block_number: self.claimed_l2_block_number,
            l2_node_address: None,
            l1_node_address: None,
            l1_beacon_address: None,
            data_dir: None,
            native: false,
            server: true,
            l2_chain_id: Some(self.l2_chain_id),
            rollup_config_path: None,
            l1_config_path: None,
            enable_experimental_witness_endpoint: true,
        }
    }
}

pub struct RiseHintHandler;

#[async_trait]
impl HintHandler for RiseHintHandler {
    type Cfg = RiseCfg;

    async fn fetch_hint(
        hint: Hint<ExtendedHintType>,
        cfg: &RiseCfg,
        providers: &SingleChainProvidersWithEigenDA,
        kv: SharedKeyValueStore,
    ) -> Result<()> {
        match hint.ty {
            ExtendedHintType::EigenDACert => {
                fetch_eigenda_hint(hint, providers, kv).await?;
            }
            ExtendedHintType::Original(ty) => {
                SingleChainHintHandler::fetch_hint(
                    Hint { ty, data: hint.data },
                    &cfg.get_legacy_cfg(),
                    &providers.kona_providers,
                    kv,
                )
                .await?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct PartialEigenDAWitnessData {
    preimage_store: PreimageStore,
    blob_data: BlobData,
    eigenda_preimage: EigenDAPreimage,
    eigenda_kzg_proofs: Vec<FixedBytes<64>>,
}

impl PartialEigenDAWitnessData {
    pub async fn build(
        backend: OnlineHostBackend<RiseCfg, RiseHintHandler>,
        l1_rpc: RpcClient,
    ) -> Result<Self> {
        let preimage = BidirectionalChannel::new()?;
        let hint = BidirectionalChannel::new()?;

        let preimage_server = PreimageServer::new(
            OracleServer::new(preimage.host),
            HintReader::new(hint.host),
            Arc::new(backend),
        );

        let server_task = tokio::task::spawn(preimage_server.start());

        let witness_generator = EigenDAWitnessGenerator::new(l1_rpc.clone(), None);
        let client_task = tokio::task::spawn(async move {
            WitnessGenerator::run(&witness_generator, preimage.client, hint.client).await
        });

        let witness = client_task.await??;

        tracing::debug!("witness ready");

        server_task.await??;

        let EigenDAWitnessData { preimage_store, blob_data, eigenda_witness } = witness;

        let (eigenda_preimage, eigenda_kzg_proofs, canoe_proof_bytes) =
            eigenda_witness.map(|w| w.into_preimage()).unwrap_or_default();
        debug_assert!(canoe_proof_bytes.is_none_or(|b| b.is_empty()));

        Ok(Self { preimage_store, blob_data, eigenda_preimage, eigenda_kzg_proofs })
    }

    pub async fn get_canoe_inputs(&self) -> Result<Vec<CanoeInput>> {
        let boot_info = BootInfo::load(&self.preimage_store).await?;
        let header_rlp =
            self.preimage_store.get(PreimageKey::new_keccak256(*boot_info.l1_head)).await?;
        let l1_head_header = alloy_consensus::Header::decode(&mut header_rlp.as_slice())?;
        let l1_chain_id = boot_info.rollup_config.l1_chain_id;

        let canoe_inputs = from_eigenda_preimage_to_canoe_inputs(
            &self.eigenda_preimage,
            CanoeVerifierAddressFetcherDeployedByEigenLabs {},
            l1_chain_id,
            boot_info.l1_head,
            l1_head_header.number,
        )?;

        Ok(canoe_inputs)
    }
}
