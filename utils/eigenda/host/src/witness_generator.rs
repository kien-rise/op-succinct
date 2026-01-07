use std::sync::{Arc, Mutex};

use alloy_primitives::FixedBytes;
use alloy_rpc_client::RpcClient;
use anyhow::Result;
use async_trait::async_trait;
use canoe_verifier_address_fetcher::CanoeVerifierAddressFetcherDeployedByEigenLabs;
use hokulea_compute_proof::compute_kzg_proof_with_srs;
use hokulea_proof::{
    eigenda_provider::OracleEigenDAPreimageProvider,
    eigenda_witness::{EigenDAPreimage, EigenDAWitness},
};
use hokulea_witgen::witness_provider::OracleEigenDAPreimageProviderWithPreimage;
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
use rust_kzg_bn254_prover::srs::SRS;
use sp1_core_executor::SP1ReduceProof;
use sp1_prover::InnerSC;
use sp1_sdk::{ProverClient, SP1Stdin};

type WitnessExecutor = EigenDAWitnessExecutor<
    PreimageWitnessCollector<DefaultOracleBase>,
    OnlineBlobStore<OracleBlobProvider<DefaultOracleBase>>,
    OracleEigenDAPreimageProvider<DefaultOracleBase>,
>;

pub struct EigenDAWitnessGenerator {
    l1_rpc_client: RpcClient,
    mock_mode: bool,
    srs: SRS<'static>,
}

impl EigenDAWitnessGenerator {
    pub fn new(l1_rpc_client: RpcClient, mock_mode: bool) -> Self {
        let srs = SRS::new(
            std::env::var("SRS_FILE").as_deref().unwrap_or("resources/g1.point"),
            268435456,
            524288,
        )
        .expect("missing srs file");
        Self { l1_rpc_client, mock_mode, srs }
    }
}

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
        tracing::debug!("Starting EigenDAWitnessGenerator::run");

        let preimage_witness_store = Arc::new(std::sync::Mutex::new(PreimageStore::default()));
        let blob_data = Arc::new(std::sync::Mutex::new(BlobData::default()));

        let preimage_oracle = Arc::new(kona_proof::CachingOracle::new(
            2048,
            OracleReader::new(preimage_chan),
            HintWriter::new(hint_chan),
        ));
        let blob_provider = OracleBlobProvider::new(preimage_oracle.clone());

        let oracle = Arc::new(PreimageWitnessCollector {
            preimage_oracle: preimage_oracle.clone(),
            preimage_witness_store: preimage_witness_store.clone(),
        });
        let beacon = OnlineBlobStore { provider: blob_provider.clone(), store: blob_data.clone() };

        // Create EigenDA blob provider that collects witness data
        let eigenda_preimage_provider = OracleEigenDAPreimageProvider::new(oracle.clone());
        let eigenda_preimage = Arc::new(Mutex::new(EigenDAPreimage::default()));

        let eigenda_preimage_provider = OracleEigenDAPreimageProviderWithPreimage {
            provider: eigenda_preimage_provider,
            preimage: eigenda_preimage.clone(),
        };

        let executor = EigenDAWitnessExecutor::new(eigenda_preimage_provider);

        tracing::debug!("Getting inputs for pipeline");
        let (boot_info, input) = get_inputs_for_pipeline(oracle.clone()).await?;
        if let Some((cursor, l1_provider, l2_provider)) = input {
            tracing::debug!("Creating pipeline");
            let rollup_config = Arc::new(boot_info.rollup_config.clone());
            let l1_config = Arc::new(boot_info.l1_config.clone());
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
            .await?;
            tracing::debug!("Running witness executor");
            WitnessExecutorTrait::run(&executor, boot_info.clone(), pipeline, cursor, l2_provider)
                .await?;
            tracing::debug!("Witness executor completed");
        }

        // Extract the EigenDA preimage data
        tracing::debug!("Extracting EigenDA preimage data");
        let eigenda_preimage_data = std::mem::take(&mut *eigenda_preimage.lock().unwrap());

        tracing::debug!(
            "Generating KZG proofs for {} encoded payloads",
            eigenda_preimage_data.encoded_payloads.len()
        );
        let mut kzg_proofs = vec![];
        for (_, encoded_payload) in &eigenda_preimage_data.encoded_payloads {
            let kzg_proof = compute_kzg_proof_with_srs(encoded_payload.serialize(), &self.srs)
                .expect("cannot generate a kzg proof");
            let bytes: FixedBytes<64> = FixedBytes::from_slice(kzg_proof.as_ref());
            kzg_proofs.push(bytes);
        }
        tracing::debug!("Generated {} KZG proofs", kzg_proofs.len());

        // Generate canoe proofs using the reduced proof provider for proof aggregation
        tracing::debug!("Generating canoe proofs (mock_mode: {})", self.mock_mode);
        use canoe_sp1_cc_host::CanoeSp1CCReducedProofProvider;
        let canoe_provider = CanoeSp1CCReducedProofProvider {
            eth_rpc_client: self.l1_rpc_client.clone(),
            mock_mode: self.mock_mode,
        };
        let maybe_canoe_proof = hokulea_witgen::from_boot_info_to_canoe_proof(
            &boot_info,
            &eigenda_preimage_data,
            oracle.clone(),
            canoe_provider,
            CanoeVerifierAddressFetcherDeployedByEigenLabs {},
        )
        .await?;
        tracing::debug!(
            "Canoe proof generation completed (proof present: {})",
            maybe_canoe_proof.is_some()
        );

        let maybe_canoe_proof_bytes =
            maybe_canoe_proof.map(|proof| serde_cbor::to_vec(&proof).expect("serde error"));

        tracing::debug!("Creating EigenDA witness from preimage data");
        let eigenda_witness = EigenDAWitness::from_preimage(
            eigenda_preimage_data,
            kzg_proofs,
            maybe_canoe_proof_bytes,
        )?;

        tracing::debug!("Serializing EigenDA witness data");
        let eigenda_witness_bytes =
            serde_cbor::to_vec(&eigenda_witness).expect("Failed to serialize EigenDA witness data");

        tracing::debug!("Creating final witness data");
        let witness = EigenDAWitnessData {
            preimage_store: preimage_witness_store.lock().unwrap().clone(),
            blob_data: blob_data.lock().unwrap().clone(),
            eigenda_data: Some(eigenda_witness_bytes),
        };

        tracing::debug!("EigenDAWitnessGenerator::run completed successfully");
        Ok(witness)
    }
}
