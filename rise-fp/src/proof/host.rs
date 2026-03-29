use std::{collections::HashMap, sync::Arc};

use alloy_primitives::{BlockNumber, ChainId, B256, U256};
use alloy_provider::RootProvider;
use alloy_rpc_client::RpcClient;
use alloy_transport_http::reqwest::Url;
use anyhow::Result;
use async_trait::async_trait;
use hokulea_host_bin::{
    cfg::SingleChainProvidersWithEigenDA, eigenda_preimage::OnlineEigenDAPreimageProvider,
    handler::fetch_eigenda_hint,
};
use hokulea_proof::hint::ExtendedHintType;
use kona_genesis::{L1ChainConfig, RollupConfig};
use kona_host::{
    single::{SingleChainHintHandler, SingleChainHost, SingleChainProviders},
    HintHandler, MemoryKeyValueStore, OnlineHostBackend, OnlineHostBackendCfg, PreimageServer,
    SharedKeyValueStore, SplitKeyValueStore,
};
use kona_preimage::{BidirectionalChannel, HintReader, OracleServer, PreimageKey};
use kona_proof::{boot, Hint, HintType};
use kona_providers_alloy::{OnlineBeaconClient, OnlineBlobProvider};
use op_succinct_client_utils::witness::EigenDAWitnessData;
use op_succinct_eigenda_host_utils::witness_generator::EigenDAWitnessGenerator;
use op_succinct_host_utils::witness_generation::traits::WitnessGenerator;
use tokio::sync::RwLock;

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

pub async fn build_partial_witness(
    backend: OnlineHostBackend<RiseCfg, RiseHintHandler>,
    l1_rpc: RpcClient,
) -> Result<EigenDAWitnessData> {
    let preimage = BidirectionalChannel::new()?;
    let hint = BidirectionalChannel::new()?;

    let preimage_server = PreimageServer::new(
        OracleServer::new(preimage.host),
        HintReader::new(hint.host),
        Arc::new(backend),
    );

    let witness_generator = EigenDAWitnessGenerator::new(l1_rpc.clone(), None);
    let server_task = tokio::task::spawn(preimage_server.start());
    let client_task = tokio::task::spawn(async move {
        WitnessGenerator::run(&witness_generator, preimage.client, hint.client).await
    });
    let witness = client_task.await??;
    server_task.await??;

    Ok(witness)
}

pub async fn create_backend(
    cfg: RiseCfg,
    l1_rpc: RpcClient,
    l2_rpc: RpcClient,
    l1_beacon_address: String,
    eigenda_proxy_address: Url,
) -> OnlineHostBackend<RiseCfg, RiseHintHandler> {
    let kv = {
        let local_kv_store = cfg.get_local_key_value_store();
        let mem_kv_store = MemoryKeyValueStore::new();
        let split_kv_store = SplitKeyValueStore::new(local_kv_store, mem_kv_store);
        Arc::new(RwLock::new(split_kv_store))
    };

    let providers = {
        SingleChainProvidersWithEigenDA {
            kona_providers: SingleChainProviders {
                l1: RootProvider::new(l1_rpc.clone()),
                l2: RootProvider::new(l2_rpc.clone()),
                blobs: OnlineBlobProvider::init(OnlineBeaconClient::new_http(l1_beacon_address))
                    .await,
            },
            eigenda_preimage_provider: OnlineEigenDAPreimageProvider::new_http(
                eigenda_proxy_address,
            ),
        }
    };

    OnlineHostBackend::new(cfg, kv, providers, RiseHintHandler)
        .with_proactive_hint(ExtendedHintType::Original(HintType::L2PayloadWitness))
}
