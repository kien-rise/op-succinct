use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use alloy_primitives::{Address, BlockNumber, B256};
use anyhow::{bail, Result};
use kona_host::{DiskKeyValueStore, KeyValueStore, MemoryKeyValueStore};
use op_succinct_client_utils::witness::{DefaultWitnessData, EigenDAWitnessData, WitnessData};
use op_succinct_eigenda_host_utils::{
    host::EigenDAOPSuccinctHost, witness_generator::EigenDAWitnessGenerator,
};
use op_succinct_host_utils::{fetcher::OPSuccinctDataFetcher, host::OPSuccinctHost};
use tokio::sync::{Mutex, OnceCell};

type WitnessPrecacherInFlightTasks = HashMap<B256, Arc<OnceCell<()>>>;

struct WitnessPrecacher<KV: KeyValueStore> {
    store: Arc<Mutex<KV>>,
    tasks: Arc<Mutex<WitnessPrecacherInFlightTasks>>,
    fetcher: Option<Arc<OPSuccinctDataFetcher>>,
}

impl WitnessPrecacher<DiskKeyValueStore> {
    fn new_disk(data_directory: PathBuf) -> Self {
        Self {
            store: Arc::new(Mutex::new(DiskKeyValueStore::new(data_directory))),
            tasks: Arc::new(Mutex::new(WitnessPrecacherInFlightTasks::new())),
            fetcher: None,
        }
    }
}

impl WitnessPrecacher<MemoryKeyValueStore> {
    fn new_memory() -> Self {
        Self {
            store: Arc::new(Mutex::new(MemoryKeyValueStore::new())),
            tasks: Arc::new(Mutex::new(WitnessPrecacherInFlightTasks::new())),
            fetcher: None,
        }
    }
}

impl<KV: KeyValueStore> WitnessPrecacher<KV> {
    fn with_fetcher(mut self, fetcher: Arc<OPSuccinctDataFetcher>) -> Self {
        self.fetcher = Some(fetcher);
        self
    }

    fn encode_key(l2_start_block: BlockNumber, l2_end_block: BlockNumber) -> B256 {
        let mut bytes = [0u8; 32];
        bytes[0] = 0xca;
        bytes[16..24].copy_from_slice(&l2_start_block.to_be_bytes());
        bytes[24..32].copy_from_slice(&l2_end_block.to_be_bytes());
        B256::from(bytes)
    }

    async fn ensure_witness_precached(
        &self,
        l2_start_block: BlockNumber,
        l2_end_block: BlockNumber,
    ) -> Result<B256> {
        let key = Self::encode_key(l2_start_block, l2_end_block);

        let cell = (self.tasks.lock().await)
            .entry(key.clone())
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone();

        let fetcher = match &self.fetcher {
            Some(fetcher) => Arc::clone(&fetcher),
            None => bail!("fetcher not set"),
        };

        cell.get_or_try_init::<anyhow::Error, _, _>(async move || {
            let l1_rpc_client = fetcher.rpc_config.l1_rpc_client();
            let witness_generator = Arc::new(EigenDAWitnessGenerator::new(l1_rpc_client, true));
            let host = EigenDAOPSuccinctHost { fetcher, witness_generator };
            let host_args = host.fetch(l2_start_block, l2_end_block, None, false).await?;
            let (preimage_store, blob_data) = host.run(&host_args).await?.into_parts();
            let witness = DefaultWitnessData::from_parts(preimage_store, blob_data);
            let buffer = rkyv::to_bytes::<rkyv::rancor::Error>(&witness)?;
            (self.store.lock().await).set(key, buffer.into_vec())?;
            Ok(())
        })
        .await?;

        Ok(key)
    }

    async fn get_precached_witness(
        &self,
        l2_start_block: BlockNumber,
        l2_end_block: BlockNumber,
    ) -> Result<DefaultWitnessData> {
        let key = self.ensure_witness_precached(l2_start_block, l2_end_block).await?;
        let buffer = (self.store.lock().await)
            .get(key)
            .ok_or_else(|| anyhow::anyhow!("db entry disappeared"))?;
        let witness_data = rkyv::from_bytes::<DefaultWitnessData, rkyv::rancor::Error>(&buffer)?;
        Ok(witness_data)
    }
}

#[cfg(all(test, feature = "integration"))]
mod tests {
    use super::*;
    use clap::Parser;

    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn get_precached_witness_integration() -> Result<()> {
        #[derive(Parser, Debug)]
        struct Args {
            l2_start_block: BlockNumber,
            l2_end_block: BlockNumber,
        }

        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .init();
        let args = Args::parse_from(std::env::args_os().skip_while(|arg| arg != "--"));

        let fetcher = Arc::new(OPSuccinctDataFetcher::new_with_rollup_config().await?);
        let witness_precacher = WitnessPrecacher::new_memory().with_fetcher(fetcher);
        let witness =
            witness_precacher.get_precached_witness(args.l2_start_block, args.l2_end_block).await?;
        tracing::info!(
            "witness: ({}, {}, {}, {})",
            witness.preimage_store.preimage_map.len(),
            witness.blob_data.blobs.len(),
            witness.blob_data.commitments.len(),
            witness.blob_data.proofs.len(),
        );
        Ok(())
    }
}
