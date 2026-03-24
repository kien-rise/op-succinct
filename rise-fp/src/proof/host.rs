use std::{collections::HashMap, sync::Arc};

use alloy_primitives::{BlockNumber, ChainId, B256, U256};
use anyhow::Result;
use async_trait::async_trait;
use hokulea_host_bin::{cfg::SingleChainProvidersWithEigenDA, handler::fetch_eigenda_hint};
use hokulea_proof::hint::ExtendedHintType;
use kona_genesis::{L1ChainConfig, RollupConfig};
use kona_host::{
    single::{SingleChainHintHandler, SingleChainHost},
    HintHandler, MemoryKeyValueStore, OnlineHostBackendCfg, SharedKeyValueStore,
};
use kona_preimage::PreimageKey;
use kona_proof::{boot, Hint};

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
