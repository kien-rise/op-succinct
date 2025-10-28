use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use kona_preimage::{HintWriter, NativeChannel, OracleReader};
use kona_proof::{
    l1::{OracleBlobProvider, OracleL1ChainProvider},
    l2::OracleL2ChainProvider,
    CachingOracle,
};
use op_succinct_client_utils::witness::{
    executor::{get_inputs_for_pipeline, WitnessExecutor},
    preimage_store::PreimageStore,
    BlobData, WitnessData,
};
use sp1_sdk::SP1Stdin;
use tracing::debug;

use crate::witness_generation::{OnlineBlobStore, PreimageWitnessCollector};

pub type DefaultOracleBase = CachingOracle<OracleReader<NativeChannel>, HintWriter<NativeChannel>>;

#[async_trait]
pub trait WitnessGenerator {
    type WitnessData: WitnessData;
    type WitnessExecutor: WitnessExecutor<
            O = PreimageWitnessCollector<DefaultOracleBase>,
            B = OnlineBlobStore<OracleBlobProvider<DefaultOracleBase>>,
            L1 = OracleL1ChainProvider<PreimageWitnessCollector<DefaultOracleBase>>,
            L2 = OracleL2ChainProvider<PreimageWitnessCollector<DefaultOracleBase>>,
        > + Sync
        + Send;

    fn get_executor(&self) -> &Self::WitnessExecutor;

    async fn run(
        &self,
        preimage_chan: NativeChannel,
        hint_chan: NativeChannel,
    ) -> Result<Self::WitnessData> {
        debug!("WitnessGenerator::run started");

        debug!("Creating preimage witness store and blob data");
        let preimage_witness_store = Arc::new(Mutex::new(PreimageStore::default()));
        let blob_data = Arc::new(Mutex::new(BlobData::default()));
        debug!("Created stores successfully");

        debug!("Creating caching oracle with cache size 2048");
        let preimage_oracle = Arc::new(CachingOracle::new(
            2048,
            OracleReader::new(preimage_chan),
            HintWriter::new(hint_chan),
        ));
        debug!("Created caching oracle successfully");

        debug!("Creating blob provider");
        let blob_provider = OracleBlobProvider::new(preimage_oracle.clone());
        debug!("Created blob provider successfully");

        debug!("Creating PreimageWitnessCollector");
        let oracle = Arc::new(PreimageWitnessCollector {
            preimage_oracle: preimage_oracle.clone(),
            preimage_witness_store: preimage_witness_store.clone(),
        });
        debug!("Created PreimageWitnessCollector successfully");

        debug!("Creating OnlineBlobStore beacon");
        let beacon = OnlineBlobStore { provider: blob_provider.clone(), store: blob_data.clone() };
        debug!("Created OnlineBlobStore successfully");

        debug!("Calling get_inputs_for_pipeline");
        let (boot_info, input) = match get_inputs_for_pipeline(oracle.clone()).await {
            Ok(result) => {
                debug!("get_inputs_for_pipeline succeeded");
                result
            }
            Err(e) => {
                debug!("get_inputs_for_pipeline failed: {:?}", e);
                return Err(e);
            }
        };
        if let Some((cursor, l1_provider, l2_provider)) = input {
            debug!("Processing pipeline input - creating configs");
            let rollup_config = Arc::new(boot_info.rollup_config.clone());
            let l1_config = Arc::new(boot_info.l1_config.clone());
            debug!("Created rollup and L1 configs");

            debug!("Creating pipeline via executor");
            let pipeline = match self
                .get_executor()
                .create_pipeline(
                    rollup_config,
                    l1_config,
                    cursor.clone(),
                    oracle.clone(),
                    beacon,
                    l1_provider.clone(),
                    l2_provider.clone(),
                )
                .await
            {
                Ok(pipeline) => {
                    debug!("Pipeline created successfully");
                    pipeline
                }
                Err(e) => {
                    debug!("Pipeline creation failed: {:?}", e);
                    return Err(e);
                }
            };

            debug!("Running executor with pipeline");
            match self.get_executor().run(boot_info, pipeline, cursor, l2_provider).await {
                Ok(_) => {
                    debug!("Executor run completed successfully");
                }
                Err(e) => {
                    debug!("Executor run failed: {:?}", e);
                    return Err(e);
                }
            };
        } else {
            debug!("No pipeline input to process");
        }

        debug!("Creating witness data from collected parts");
        let preimage_store = match preimage_witness_store.lock() {
            Ok(store) => {
                debug!("Successfully locked preimage store, entries: {}", store.preimage_map.len());
                store.clone()
            }
            Err(e) => {
                debug!("Failed to lock preimage store: {:?}", e);
                return Err(anyhow!("Failed to lock preimage store: {:?}", e));
            }
        };

        let blob_data_inner = match blob_data.lock() {
            Ok(data) => {
                debug!("Successfully locked blob data");
                data.clone()
            }
            Err(e) => {
                debug!("Failed to lock blob data: {:?}", e);
                return Err(anyhow!("Failed to lock blob data: {:?}", e));
            }
        };

        let witness = Self::WitnessData::from_parts(preimage_store, blob_data_inner);
        debug!("Witness data created successfully, WitnessGenerator::run completed");

        Ok(witness)
    }

    fn get_sp1_stdin(&self, witness: Self::WitnessData) -> Result<SP1Stdin>;
}
