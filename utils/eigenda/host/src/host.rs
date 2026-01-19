use std::{collections::HashMap, sync::Arc};

use alloy_eips::BlockId;
use alloy_primitives::B256;
use anyhow::{Context, Result};
use async_trait::async_trait;
use hokulea_host_bin::cfg::SingleChainHostWithEigenDA;
use hokulea_proof::eigenda_witness::EigenDAWitness;
use kona_host::{DiskKeyValueStore, KeyValueStore, MemoryKeyValueStore};
use kona_proof::BootInfo;
use op_succinct_client_utils::witness::EigenDAWitnessData;
use op_succinct_host_utils::{
    fetcher::OPSuccinctDataFetcher, host::OPSuccinctHost, witness_generation::WitnessGenerator,
};
use sp1_sdk::SP1Stdin;
use tokio::sync::{Mutex, OnceCell};

use crate::witness_generator::EigenDAWitnessGenerator;

type WitnessPrecacherInFlightTasks = HashMap<B256, Arc<OnceCell<()>>>;

#[derive(Clone)]
pub struct EigenDAOPSuccinctHost {
    pub fetcher: Arc<OPSuccinctDataFetcher>,
    pub witness_generator: Arc<EigenDAWitnessGenerator>,
    tasks: Option<Arc<Mutex<WitnessPrecacherInFlightTasks>>>,
    store: Option<Arc<Mutex<Box<dyn KeyValueStore + Send + Sync>>>>,
}

#[async_trait]
impl OPSuccinctHost for EigenDAOPSuccinctHost {
    type Args = SingleChainHostWithEigenDA;
    type WitnessGenerator = EigenDAWitnessGenerator;

    fn witness_generator(&self) -> &Self::WitnessGenerator {
        &self.witness_generator
    }

    async fn fetch(
        &self,
        l2_start_block: u64,
        l2_end_block: u64,
        l1_head_hash: Option<B256>,
        safe_db_fallback: bool,
    ) -> Result<SingleChainHostWithEigenDA> {
        // If l1_head_hash is not provided, calculate a safe L1 head
        let l1_head = match l1_head_hash {
            Some(hash) => hash,
            None => {
                self.calculate_safe_l1_head(&self.fetcher, l2_end_block, safe_db_fallback).await?
            }
        };

        let host = self.fetcher.get_host_args(l2_start_block, l2_end_block, l1_head).await?;

        let eigenda_proxy_address = std::env::var("EIGENDA_PROXY_ADDRESS").ok();
        Ok(SingleChainHostWithEigenDA { kona_cfg: host, eigenda_proxy_address, verbose: 1 })
    }

    async fn range_proof_stdin(
        &self,
        start_block: u64,
        end_block: u64,
        safe_db_fallback: bool,
    ) -> Result<SP1Stdin> {
        // - Fallback to the default impl if store is None
        let Some(store) = &self.store else {
            return OPSuccinctHost::range_proof_stdin(
                self,
                start_block,
                end_block,
                safe_db_fallback,
            )
            .await;
        };

        // - Get the precached witness
        let key = self.precache_witness(start_block, end_block, safe_db_fallback).await?;

        let witness = match store.lock().await.get(key) {
            Some(buffer) => rkyv::from_bytes::<EigenDAWitnessData, rkyv::rancor::Error>(&buffer)?,
            None => anyhow::bail!("db entry disappeared"),
        };

        // - Complete the witness
        let witness_data = self.complete_witness(witness).await?;

        let sp1_stdin = self
            .witness_generator
            .get_sp1_stdin(witness_data)
            .context("Failed to get proof stdin")?;

        Ok(sp1_stdin)
    }

    fn get_l1_head_hash(&self, args: &Self::Args) -> Option<B256> {
        Some(args.kona_cfg.l1_head)
    }

    async fn get_finalized_l2_block_number(
        &self,
        fetcher: &OPSuccinctDataFetcher,
        _: u64,
    ) -> Result<Option<u64>> {
        let finalized_l2_block_number = fetcher.get_l2_header(BlockId::finalized()).await?;
        Ok(Some(finalized_l2_block_number.number))
    }

    async fn calculate_safe_l1_head(
        &self,
        fetcher: &OPSuccinctDataFetcher,
        l2_end_block: u64,
        safe_db_fallback: bool,
    ) -> Result<B256> {
        // For EigenDA, use a similar approach to Ethereum DA with a conservative offset.
        let (_, l1_head_number) = fetcher.get_l1_head(l2_end_block, safe_db_fallback).await?;

        // Add a buffer for EigenDA similar to Ethereum DA.
        let l1_head_number = l1_head_number + 20;

        // Ensure we don't exceed the finalized L1 header.
        let finalized_l1_header = fetcher.get_l1_header(BlockId::finalized()).await?;
        let safe_l1_head_number = std::cmp::min(l1_head_number, finalized_l1_header.number);

        Ok(fetcher.get_l1_header(safe_l1_head_number.into()).await?.hash_slow())
    }
}

impl EigenDAOPSuccinctHost {
    pub fn new(fetcher: Arc<OPSuccinctDataFetcher>) -> Self {
        let l1_rpc_client = fetcher.rpc_config.l1_rpc_client();
        let mock_mode = std::env::var("OP_SUCCINCT_MOCK")
            .unwrap_or("false".to_string())
            .parse::<bool>()
            .unwrap_or(false);
        let store: Option<Arc<Mutex<Box<dyn KeyValueStore + Send + Sync>>>> =
            match std::env::var("WITNESS_PRECACHER_DATA_DIR") {
                Ok(dir) => {
                    if dir.is_empty() {
                        Some(Arc::new(Mutex::new(Box::new(MemoryKeyValueStore::new()))))
                    } else {
                        Some(Arc::new(Mutex::new(Box::new(DiskKeyValueStore::new(dir.into())))))
                    }
                }
                Err(_) => None,
            };
        let tasks = if store.is_some() {
            Some(Arc::new(Mutex::new(WitnessPrecacherInFlightTasks::new())))
        } else {
            None
        };
        Self {
            fetcher,
            witness_generator: Arc::new(EigenDAWitnessGenerator::new(l1_rpc_client, mock_mode)),
            tasks,
            store,
        }
    }

    // TODO: call this
    pub async fn precache_witness(
        &self,
        start_block: u64,
        end_block: u64,
        safe_db_fallback: bool,
    ) -> Result<B256> {
        let (Some(tasks), Some(store)) = (&self.tasks, &self.store) else {
            anyhow::bail!("Cannot precache witness without configured storage")
        };

        // - Calculate cache key
        let key = {
            let mut bytes = [0u8; 32];
            bytes[0] = 0xca;
            bytes[16..24].copy_from_slice(&start_block.to_be_bytes());
            bytes[24..32].copy_from_slice(&end_block.to_be_bytes());
            B256::from(bytes)
        };

        // - Ensure witness is precached
        let cell = (tasks.lock().await)
            .entry(key.clone())
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone();

        cell.get_or_try_init::<anyhow::Error, _, _>(async move || {
            let host = EigenDAOPSuccinctHost {
                fetcher: Arc::clone(&self.fetcher),
                witness_generator: Arc::new(EigenDAWitnessGenerator::new(
                    self.fetcher.rpc_config.l1_rpc_client(),
                    true, // force mock mode
                )),
                tasks: None,
                store: None,
            };
            let host_args = host.fetch(start_block, end_block, None, safe_db_fallback).await?;
            let witness = host.run(&host_args).await?;
            let buffer = rkyv::to_bytes::<rkyv::rancor::Error>(&witness)?;
            (store.lock().await).set(key, buffer.into_vec())?;
            Ok(())
        })
        .await?;

        Ok(key)
    }

    pub async fn complete_witness(
        &self,
        mut witness: EigenDAWitnessData,
    ) -> Result<EigenDAWitnessData> {
        // - Take witness.eigenda_witness
        let (eigenda_preimage_data, kzg_proofs, _) = match witness.eigenda_witness.take() {
            Some(w) => w.into_preimage(),
            None => anyhow::bail!("missing eigenda_witness"),
        };

        // - Compute the real eigenda_witness
        let boot_info = BootInfo::load(&witness.preimage_store).await?;

        use canoe_sp1_cc_host::CanoeSp1CCReducedProofProvider;
        let canoe_provider = CanoeSp1CCReducedProofProvider {
            eth_rpc_client: self.fetcher.rpc_config.l1_rpc_client(),
            mock_mode: false,
        };

        use canoe_verifier_address_fetcher::CanoeVerifierAddressFetcherDeployedByEigenLabs;
        let maybe_canoe_proof = hokulea_witgen::from_boot_info_to_canoe_proof(
            &boot_info,
            &eigenda_preimage_data,
            &witness.preimage_store,
            canoe_provider,
            CanoeVerifierAddressFetcherDeployedByEigenLabs {},
        )
        .await?;

        let maybe_canoe_proof_bytes =
            maybe_canoe_proof.map(|proof| serde_cbor::to_vec(&proof).expect("serde error"));

        // - Update witness.eigenda_witness
        let eigenda_witness = EigenDAWitness::from_preimage(
            eigenda_preimage_data,
            kzg_proofs,
            maybe_canoe_proof_bytes,
        )?;

        witness.eigenda_witness = Some(eigenda_witness);
        Ok(witness)
    }
}
