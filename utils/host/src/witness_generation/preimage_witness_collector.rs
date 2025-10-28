use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kona_preimage::{
    errors::PreimageOracleResult, CommsClient, HintWriterClient, PreimageKey, PreimageOracleClient,
};
use kona_proof::FlushableCache;
use op_succinct_client_utils::witness::preimage_store::PreimageStore;
use tracing::debug;

#[derive(Clone, Debug)]
pub struct PreimageWitnessCollector<P: CommsClient + FlushableCache + Send + Sync + Clone> {
    pub preimage_oracle: Arc<P>,
    pub preimage_witness_store: Arc<Mutex<PreimageStore>>,
}

#[async_trait]
impl<P> PreimageOracleClient for PreimageWitnessCollector<P>
where
    P: CommsClient + FlushableCache + Send + Sync + Clone,
{
    async fn get(&self, key: PreimageKey) -> PreimageOracleResult<Vec<u8>> {
        debug!("PreimageWitnessCollector::get called with key={:?}", key);

        debug!("Calling preimage_oracle.get");
        let value = match self.preimage_oracle.get(key).await {
            Ok(val) => {
                debug!("preimage_oracle.get succeeded, value_len={}", val.len());
                val
            }
            Err(e) => {
                debug!("preimage_oracle.get failed: {:?}", e);
                return Err(e);
            }
        };

        debug!("Calling save to store the preimage");
        self.save(key, &value);
        debug!("save completed, returning value");

        Ok(value)
    }

    async fn get_exact(&self, key: PreimageKey, buf: &mut [u8]) -> PreimageOracleResult<()> {
        debug!(
            "PreimageWitnessCollector::get_exact called with key={:?}, buf_len={}",
            key,
            buf.len()
        );

        debug!("Calling preimage_oracle.get_exact");
        match self.preimage_oracle.get_exact(key, buf).await {
            Ok(()) => {
                debug!("preimage_oracle.get_exact succeeded");
            }
            Err(e) => {
                debug!("preimage_oracle.get_exact failed: {:?}", e);
                return Err(e);
            }
        };

        debug!("Calling save to store the preimage from buffer");
        self.save(key, buf);
        debug!("save completed, get_exact finished successfully");

        Ok(())
    }
}

#[async_trait]
impl<P> HintWriterClient for PreimageWitnessCollector<P>
where
    P: CommsClient + FlushableCache + Send + Sync + Clone,
{
    async fn write(&self, hint: &str) -> PreimageOracleResult<()> {
        self.preimage_oracle.write(hint).await
    }
}

impl<P> FlushableCache for PreimageWitnessCollector<P>
where
    P: CommsClient + FlushableCache + Send + Sync + Clone,
{
    fn flush(&self) {
        self.preimage_oracle.flush();
    }
}

impl<P> PreimageWitnessCollector<P>
where
    P: CommsClient + FlushableCache + Send + Sync + Clone,
{
    pub fn save(&self, key: PreimageKey, value: &[u8]) {
        debug!(
            "PreimageWitnessCollector::save called with key={:?}, value_len={}",
            key,
            value.len()
        );

        debug!("Converting value slice to Vec");
        let value_vec = value.to_vec();
        debug!("Value conversion completed, vec_len={}", value_vec.len());

        debug!("Attempting to acquire mutex lock on preimage_witness_store");
        let mut store = match self.preimage_witness_store.lock() {
            Ok(store) => {
                debug!("Successfully acquired mutex lock");
                store
            }
            Err(e) => {
                debug!("Failed to acquire mutex lock: {:?}", e);
                panic!("Failed to acquire mutex lock: {:?}", e);
            }
        };

        debug!("Calling save_preimage on store");
        store.save_preimage(key, value_vec);
        debug!("save_preimage completed successfully");

        // Explicitly drop the lock
        drop(store);
        debug!("Mutex lock released, PreimageWitnessCollector::save completed");
    }
}
