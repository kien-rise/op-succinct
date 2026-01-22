use std::{collections::HashMap, sync::Arc};

use alloy_eips::BlockId;
use alloy_primitives::B256;
use alloy_rlp::Decodable;
use anyhow::{Context, Result};
use async_trait::async_trait;
use canoe_sp1_cc_host::{canoe_proof_stdin, generate_canoe_proof};
use canoe_verifier_address_fetcher::CanoeVerifierAddressFetcherDeployedByEigenLabs;
use hokulea_host_bin::cfg::SingleChainHostWithEigenDA;
use hokulea_proof::eigenda_witness::EigenDAWitness;
use hokulea_witgen::from_eigenda_preimage_to_canoe_inputs;
use kona_host::{DiskKeyValueStore, KeyValueStore, MemoryKeyValueStore};
use kona_preimage::{PreimageKey, PreimageOracleClient};
use kona_proof::BootInfo;
use op_succinct_client_utils::witness::EigenDAWitnessData;
use op_succinct_host_utils::{
    fetcher::OPSuccinctDataFetcher, host::OPSuccinctHost, witness_generation::WitnessGenerator,
};
use sp1_sdk::{SP1Proof, SP1Stdin};
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
        tracing::debug!(
            "range_proof_stdin: start_block={}, end_block={}, safe_db_fallback={}",
            start_block,
            end_block,
            safe_db_fallback
        );

        // - Fallback to the default impl if store is None
        let Some(store) = &self.store else {
            tracing::debug!(
                "range_proof_stdin: no store configured, falling back to default implementation"
            );
            return OPSuccinctHost::range_proof_stdin(
                self,
                start_block,
                end_block,
                safe_db_fallback,
            )
            .await;
        };

        // - Get the precached witness
        tracing::debug!("range_proof_stdin: precaching witness");
        let key = self.precache_witness(start_block, end_block, safe_db_fallback).await?;
        tracing::debug!("range_proof_stdin: witness precached with key={:?}", key);

        tracing::debug!("range_proof_stdin: retrieving witness from store");
        let (mut witness, maybe_canoe_stdin): (EigenDAWitnessData, Option<SP1Stdin>) =
            match store.lock().await.get(key) {
                Some(buffer) => {
                    tracing::debug!(
                        "range_proof_stdin: deserializing witness from buffer (size={})",
                        buffer.len()
                    );
                    serde_cbor::from_slice(&buffer)?
                }
                None => anyhow::bail!("db entry disappeared"),
            };

        // - Complete the witness
        tracing::debug!("range_proof_stdin: completing witness");
        self.complete_witness(&mut witness, maybe_canoe_stdin).await?;
        tracing::debug!("range_proof_stdin: witness completed");

        tracing::debug!("range_proof_stdin: generating SP1 stdin");
        let sp1_stdin =
            self.witness_generator.get_sp1_stdin(witness).context("Failed to get proof stdin")?;
        tracing::debug!("range_proof_stdin: SP1 stdin generated successfully");

        Ok(sp1_stdin)
    }

    async fn is_witness_precached(&self, start_block: u64, end_block: u64) -> Result<bool> {
        let (Some(tasks), Some(store)) = (&self.tasks, &self.store) else {
            anyhow::bail!(
                "witness precaching is not available on this host due to unconfigured storage"
            );
        };
        let key = self.get_cache_key(start_block, end_block);
        if (tasks.lock().await).contains_key(&key) {
            return Ok(true);
        }
        if store.lock().await.get(key).is_some() {
            return Ok(true);
        }
        Ok(false)
    }

    async fn precache_witness(
        &self,
        start_block: u64,
        end_block: u64,
        safe_db_fallback: bool,
    ) -> Result<B256> {
        tracing::debug!(
            "precache_witness: start_block={}, end_block={}, safe_db_fallback={}",
            start_block,
            end_block,
            safe_db_fallback
        );

        let (Some(tasks), Some(store)) = (&self.tasks, &self.store) else {
            anyhow::bail!("Cannot precache witness without configured storage")
        };

        // - Calculate cache key
        let key = self.get_cache_key(start_block, end_block);
        tracing::debug!("precache_witness: calculated cache key={:?}", key);

        // - Ensure witness is precached
        let cell = (tasks.lock().await)
            .entry(key.clone())
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone();

        cell.get_or_try_init::<anyhow::Error, _, _>(async move || {
            // - Check if witness is already in store (e.g., after restart)
            tracing::debug!("precache_witness: checking if witness exists in store");
            if store.lock().await.get(key).is_some() {
                tracing::debug!(
                    "precache_witness: witness already exists in store, skipping generation"
                );
                return Ok(());
            }
            tracing::debug!("precache_witness: witness not in store");

            tracing::debug!("precache_witness: cache miss, generating witness");
            let host = EigenDAOPSuccinctHost {
                fetcher: Arc::clone(&self.fetcher),
                witness_generator: Arc::new(EigenDAWitnessGenerator::new(
                    self.fetcher.rpc_config.l1_rpc_client(),
                    true, // force mock mode
                )),
                tasks: None,
                store: None,
            };
            tracing::debug!("precache_witness: created host with mock mode");

            tracing::debug!("precache_witness: fetching host args");
            let host_args = host.fetch(start_block, end_block, None, safe_db_fallback).await?;
            tracing::debug!(host_args = ?host_args, "precache_witness: host args fetched successfully");

            tracing::debug!("precache_witness: running witness generation");
            let mut witness = host.run(&host_args).await?;
            tracing::debug!("precache_witness: witness generated successfully");

            tracing::debug!("precache_witness: extracting EigenDA preimage data");
            let (eigenda_preimage_data, kzg_proofs, canoe_proof) =
                match witness.eigenda_witness.take() {
                    Some(w) => w.into_preimage(),
                    None => anyhow::bail!("missing eigenda_witness"),
                };
            tracing::debug!("precache_witness: EigenDA preimage data extracted");

            let maybe_canoe_stdin = {
                tracing::debug!("precache_witness: loading boot info");
                let boot_info = BootInfo::load(&witness.preimage_store).await?;
                tracing::debug!(
                    "precache_witness: boot info loaded, l1_head={:?}, l1_chain_id={}",
                    boot_info.l1_head,
                    boot_info.rollup_config.l1_chain_id
                );

                tracing::debug!("precache_witness: fetching L1 head header from preimage store");
                let header_rlp = witness
                    .preimage_store
                    .get(PreimageKey::new_keccak256(*boot_info.l1_head))
                    .await?;
                let l1_head_header = alloy_consensus::Header::decode(&mut header_rlp.as_slice())
                    .map_err(|_| {
                        anyhow::Error::msg("cannot rlp decode header in canoe proof generation")
                    })?;
                tracing::debug!(
                    "precache_witness: L1 head header decoded, block_number={}",
                    l1_head_header.number
                );

                let l1_chain_id = boot_info.rollup_config.l1_chain_id;

                tracing::debug!("precache_witness: generating canoe inputs from EigenDA preimage");
                let canoe_inputs = from_eigenda_preimage_to_canoe_inputs(
                    &eigenda_preimage_data,
                    CanoeVerifierAddressFetcherDeployedByEigenLabs {},
                    l1_chain_id,
                    boot_info.l1_head,
                    l1_head_header.number,
                )?;
                tracing::debug!(
                    "precache_witness: canoe inputs generated, count={}",
                    canoe_inputs.len()
                );

                if canoe_inputs.is_empty() {
                    tracing::debug!(
                        "precache_witness: no canoe inputs, skipping canoe proof stdin generation"
                    );
                    None
                } else {
                    tracing::debug!("precache_witness: generating canoe proof stdin");
                    let stdin =
                        canoe_proof_stdin(&canoe_inputs, self.fetcher.rpc_config.l1_rpc_client())
                            .await?;
                    tracing::debug!("precache_witness: canoe proof stdin generated successfully");
                    Some(stdin)
                }
            };

            tracing::debug!("precache_witness: reconstructing EigenDA witness");
            witness.eigenda_witness = Some(EigenDAWitness::from_preimage(
                eigenda_preimage_data,
                kzg_proofs,
                canoe_proof,
            )?);
            tracing::debug!("precache_witness: EigenDA witness reconstructed");

            tracing::debug!("precache_witness: serializing witness and canoe stdin to buffer");
            let buffer = serde_cbor::to_vec(&(witness, maybe_canoe_stdin))?;
            tracing::debug!("precache_witness: serialized to buffer (size={})", buffer.len());

            tracing::debug!("precache_witness: storing in cache");
            (store.lock().await).set(key, buffer)?;
            tracing::debug!("precache_witness: stored in cache successfully");

            Ok(())
        })
        .await?;

        tracing::debug!("precache_witness: completed successfully, returning key={:?}", key);
        Ok(key)
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

    fn get_cache_key(&self, start_block: u64, end_block: u64) -> B256 {
        let mut bytes = [0u8; 32];
        bytes[0] = 0xc1;
        // TODO: include game hashes here
        bytes[16..24].copy_from_slice(&start_block.to_be_bytes());
        bytes[24..32].copy_from_slice(&end_block.to_be_bytes());
        B256::from(bytes)
    }

    pub async fn complete_witness(
        &self,
        witness: &mut EigenDAWitnessData,
        maybe_canoe_stdin: Option<SP1Stdin>,
    ) -> Result<()> {
        tracing::debug!(
            "complete_witness: starting witness completion, has_canoe_stdin={}",
            maybe_canoe_stdin.is_some()
        );

        let maybe_canoe_proof_bytes = if let Some(stdin) = maybe_canoe_stdin {
            tracing::debug!("complete_witness: generating canoe proof from stdin");
            let proof_result = generate_canoe_proof(stdin, false).await?;
            tracing::debug!("complete_witness: canoe proof generated");

            match proof_result.proof {
                SP1Proof::Compressed(proof) => {
                    tracing::debug!("complete_witness: serializing compressed proof");
                    let proof_bytes = serde_cbor::to_vec(proof.as_ref())?;
                    tracing::debug!(
                        "complete_witness: compressed proof serialized (size={})",
                        proof_bytes.len()
                    );
                    Some(proof_bytes)
                }
                _ => anyhow::bail!("cannot get Sp1ReducedProof"),
            }
        } else {
            tracing::debug!("complete_witness: no canoe stdin provided, skipping proof generation");
            None
        };

        tracing::debug!("complete_witness: updating witness with canoe proof bytes");
        match witness.eigenda_witness.as_mut() {
            Some(w) => {
                w.canoe_proof_bytes = maybe_canoe_proof_bytes;
                tracing::debug!("complete_witness: witness updated successfully");
            }
            None => anyhow::bail!("missing eigenda_witness"),
        }

        tracing::debug!("complete_witness: completed successfully");
        Ok(())
    }
}
