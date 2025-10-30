use std::{
    env,
    sync::{Arc, Mutex},
};

use alloy_primitives::Address;
use anyhow::Result;
use async_trait::async_trait;
use canoe_verifier_address_fetcher::{
    CanoeVerifierAddressFetcher, CanoeVerifierAddressFetcherError,
};
use eigenda_cert::EigenDAVersionedCert;
use hokulea_proof::{
    eigenda_provider::OracleEigenDAPreimageProvider, eigenda_witness::EigenDAWitness,
};
use hokulea_witgen::witness_provider::OracleEigenDAWitnessProvider;
use kona_preimage::{HintWriter, NativeChannel, OracleReader};
use kona_proof::l1::OracleBlobProvider;
use op_succinct_client_utils::witness::{
    executor::{get_inputs_for_pipeline, WitnessExecutor as WitnessExecutorTrait},
    preimage_store::PreimageStore,
    BlobData, EigenDAWitnessData,
};
use op_succinct_eigenda_client_utils::executor::EigenDAWitnessExecutor;
use op_succinct_host_utils::witness_generation::{
    online_blob_store::OnlineBlobStore, preimage_witness_collector::PreimageWitnessCollector,
    DefaultOracleBase, WitnessGenerator,
};
use rkyv::to_bytes;
use sp1_core_executor::SP1ReduceProof;
use sp1_prover::InnerSC;
use sp1_sdk::{ProverClient, SP1Stdin};

type WitnessExecutor = EigenDAWitnessExecutor<
    PreimageWitnessCollector<DefaultOracleBase>,
    OnlineBlobStore<OracleBlobProvider<DefaultOracleBase>>,
    OracleEigenDAPreimageProvider<DefaultOracleBase>,
>;

#[derive(Clone)]
pub struct CanoeVerifierAddressFetcherSingleAddress(Address);

impl CanoeVerifierAddressFetcher for CanoeVerifierAddressFetcherSingleAddress {
    fn fetch_address(
        &self,
        _chain_id: u64,
        _versioned_cert: &EigenDAVersionedCert,
    ) -> Result<Address, CanoeVerifierAddressFetcherError> {
        Ok(self.0)
    }
}

pub struct EigenDAWitnessGenerator {}

#[async_trait]
impl WitnessGenerator for EigenDAWitnessGenerator {
    type WitnessData = EigenDAWitnessData;
    type WitnessExecutor = WitnessExecutor;

    fn get_executor(&self) -> &Self::WitnessExecutor {
        panic!("get_executor should not be called directly for EigenDAWitnessGenerator")
    }

    fn get_sp1_stdin(&self, mut witness: Self::WitnessData) -> Result<SP1Stdin> {
        let mut stdin = SP1Stdin::new();

        // If eigenda blob witness data is present, write the canoe proof to stdin
        if let Some(eigenda_data) = &witness.eigenda_data {
            let mut eigenda_witness: EigenDAWitness = serde_cbor::from_slice(eigenda_data)
                .map_err(|e| {
                    anyhow::anyhow!("Failed to deserialize EigenDA blob witness data: {}", e)
                })?;

            // Take the canoe proof bytes from the witness data
            if let Some(proof_bytes) = eigenda_witness.canoe_proof_bytes.take() {
                // Get the canoe SP1 CC client ELF and setup verification key
                // The ELF is included in the canoe-sp1-cc-host crate
                const CANOE_ELF: &[u8] = canoe_sp1_cc_host::ELF;
                let client = ProverClient::from_env();
                let (_pk, canoe_vk) = client.setup(CANOE_ELF);

                let reduced_proof: SP1ReduceProof<InnerSC> =
                    serde_cbor::from_slice(&proof_bytes)
                        .map_err(|e| anyhow::anyhow!("Failed to deserialize canoe proof: {}", e))?;
                stdin.write_proof(reduced_proof, canoe_vk.vk.clone());

                // Re-serialize the witness data without the proof
                witness.eigenda_data = Some(serde_cbor::to_vec(&eigenda_witness).map_err(|e| {
                    anyhow::anyhow!("Failed to serialize sanitized EigenDA data: {}", e)
                })?);
            }
        }

        // Write the witness data after the proofs
        let buffer = to_bytes::<rkyv::rancor::Error>(&witness)?;
        stdin.write_slice(&buffer);
        Ok(stdin)
    }

    async fn run(
        &self,
        preimage_chan: NativeChannel,
        hint_chan: NativeChannel,
    ) -> Result<Self::WitnessData> {
        tracing::info!("SC: Starting EigenDA witness generator run");

        tracing::debug!("SC: Initializing preimage witness store and blob data");
        let preimage_witness_store = Arc::new(std::sync::Mutex::new(PreimageStore::default()));
        let blob_data = Arc::new(std::sync::Mutex::new(BlobData::default()));

        tracing::debug!("SC: Creating caching oracle with preimage and hint channels");
        let preimage_oracle = Arc::new(kona_proof::CachingOracle::new(
            2048,
            OracleReader::new(preimage_chan),
            HintWriter::new(hint_chan),
        ));
        let blob_provider = OracleBlobProvider::new(preimage_oracle.clone());
        tracing::debug!("SC: Successfully created oracle and blob provider");

        tracing::debug!("SC: Creating preimage witness collector and blob store");
        let oracle = Arc::new(PreimageWitnessCollector {
            preimage_oracle: preimage_oracle.clone(),
            preimage_witness_store: preimage_witness_store.clone(),
        });
        let beacon = OnlineBlobStore { provider: blob_provider.clone(), store: blob_data.clone() };

        // Create EigenDA blob provider that collects witness data
        tracing::debug!("SC: Setting up EigenDA providers and witness collection");
        let eigenda_preimage_provider = OracleEigenDAPreimageProvider::new(oracle.clone());
        let eigenda_witness = Arc::new(Mutex::new(EigenDAWitness::default()));

        let eigenda_blob_and_witness_provider = OracleEigenDAWitnessProvider {
            provider: eigenda_preimage_provider,
            witness: eigenda_witness.clone(),
        };

        let executor = EigenDAWitnessExecutor::new(eigenda_blob_and_witness_provider);
        tracing::debug!("SC: Successfully created EigenDA witness executor");

        tracing::info!("SC: Getting inputs for pipeline");
        let (boot_info, input) = get_inputs_for_pipeline(oracle.clone()).await.unwrap();
        tracing::debug!("SC: Successfully retrieved boot info and pipeline inputs");

        if let Some((cursor, l1_provider, l2_provider)) = input {
            tracing::info!("SC: Pipeline inputs available, creating and running pipeline");
            let rollup_config = Arc::new(boot_info.rollup_config.clone());
            let l1_config = Arc::new(boot_info.l1_config.clone());

            tracing::debug!("SC: Creating witness executor pipeline");
            let pipeline = WitnessExecutorTrait::create_pipeline(
                &executor,
                rollup_config,
                l1_config,
                cursor.clone(),
                oracle.clone(),
                beacon,
                l1_provider.clone(),
                l2_provider.clone(),
            )
            .await
            .unwrap();
            tracing::info!("SC: Successfully created pipeline, starting execution");

            WitnessExecutorTrait::run(&executor, boot_info.clone(), pipeline, cursor, l2_provider)
                .await
                .unwrap();
            tracing::info!("SC: Pipeline execution completed successfully");
        } else {
            tracing::warn!("SC: No pipeline inputs available, skipping pipeline execution");
        }

        // Extract the EigenDA witness data
        tracing::debug!("SC: Extracting EigenDA witness data");
        let mut eigenda_witness_data = std::mem::take(&mut *eigenda_witness.lock().unwrap());
        tracing::debug!("SC: Successfully extracted EigenDA witness data");

        // Generate canoe proofs using the reduced proof provider for proof aggregation
        tracing::info!("SC: Starting canoe proof generation");
        use canoe_sp1_cc_host::CanoeSp1CCReducedProofProvider;
        let eth_rpc_url = std::env::var("L1_RPC")
            .map_err(|_| anyhow::anyhow!("L1_RPC environment variable not set"))?;
        let mock_mode = env::var("OP_SUCCINCT_MOCK")
            .unwrap_or("false".to_string())
            .parse::<bool>()
            .unwrap_or(false);
        tracing::debug!(
            "SC: Canoe provider config - eth_rpc_url: {}, mock_mode: {}",
            eth_rpc_url,
            mock_mode
        );

        let canoe_provider = CanoeSp1CCReducedProofProvider { eth_rpc_url, mock_mode };
        tracing::debug!("SC: Generating canoe proof from boot info");
        let canoe_verifier_address = env::var("RISE_CANOE_VERIFIER_ADDRESS")?.parse::<Address>()?;
        let canoe_proofs = hokulea_witgen::from_boot_info_to_canoe_proof(
            &boot_info,
            &eigenda_witness_data,
            oracle.clone(),
            canoe_provider,
            CanoeVerifierAddressFetcherSingleAddress(canoe_verifier_address),
        )
        .await?;
        tracing::info!("SC: Canoe proof generation completed");

        if let Some(proof) = canoe_proofs {
            // Store the canoe proof in the witness data
            tracing::debug!("SC: Serializing canoe proof for witness data");
            let canoe_proof_bytes =
                serde_cbor::to_vec(&proof).expect("Failed to serialize canoe proof");
            eigenda_witness_data.canoe_proof_bytes = Some(canoe_proof_bytes);
            tracing::debug!("SC: Successfully stored canoe proof in witness data");
        } else {
            tracing::debug!("SC: No canoe proof generated");
        }

        tracing::debug!("SC: Serializing EigenDA witness data");
        let eigenda_witness_bytes = serde_cbor::to_vec(&eigenda_witness_data)
            .expect("Failed to serialize EigenDA witness data");
        tracing::debug!("SC: Successfully serialized EigenDA witness data");

        tracing::debug!("SC: Creating final witness data structure");
        let witness = EigenDAWitnessData {
            preimage_store: preimage_witness_store.lock().unwrap().clone(),
            blob_data: blob_data.lock().unwrap().clone(),
            eigenda_data: Some(eigenda_witness_bytes),
        };

        tracing::info!("SC: EigenDA witness generator run completed successfully");
        Ok(witness)
    }
}
