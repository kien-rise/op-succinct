use alloy_primitives::keccak256;
use async_trait::async_trait;
use kona_preimage::{
    errors::{PreimageOracleError, PreimageOracleResult},
    HintWriterClient, PreimageKey, PreimageKeyType, PreimageOracleClient,
};
use kona_proof::FlushableCache;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::HashMap;
use tracing::debug;

#[derive(
    Clone, Debug, Default, Serialize, Deserialize, rkyv::Serialize, rkyv::Archive, rkyv::Deserialize,
)]
pub struct PreimageStore {
    pub preimage_map: HashMap<PreimageKey, Vec<u8>>,
}

impl PreimageStore {
    pub fn check_preimages(&self) -> PreimageOracleResult<()> {
        for (key, value) in &self.preimage_map {
            check_preimage(key, value)?;
        }
        Ok(())
    }

    pub fn save_preimage(&mut self, key: PreimageKey, value: Vec<u8>) {
        debug!("save_preimage called with key={:?}, value_len={}", key, value.len());

        debug!("Checking preimage validity");
        check_preimage(&key, &value).expect("Invalid preimage");
        debug!("Preimage validation passed");

        if let Some(old) = self.preimage_map.insert(key, value.clone()) {
            debug!(
                "Key already exists, checking for consistency. old_len={}, new_len={}",
                old.len(),
                value.len()
            );
            assert_eq!(old, value, "Cannot overwrite key");
            debug!("Consistency check passed for existing key");
        } else {
            debug!("New key inserted successfully");
        }
        debug!(
            "save_preimage completed successfully, total keys in store: {}",
            self.preimage_map.len()
        );
    }
}

/// Check that the preimage matches the expected hash.
pub fn check_preimage(key: &PreimageKey, value: &[u8]) -> PreimageOracleResult<()> {
    if value.len() < 256 {
        debug!(
            "check_preimage called with key={:?}, value_len={}, value={:02x?}",
            key,
            value.len(),
            value
        );
    } else {
        debug!("check_preimage called with key={:?}, value_len={}", key, value.len());
    }
    debug!("Key type: {:?}", key.key_type());

    if let Some(expected_hash) = match key.key_type() {
        PreimageKeyType::Keccak256 => {
            debug!("Computing Keccak256 hash for value");
            let hash = keccak256(value).0;
            debug!("Computed Keccak256 hash: {:?}", hash);
            Some(hash)
        }
        PreimageKeyType::Sha256 => {
            debug!("Computing SHA256 hash for value");
            let hash: [u8; 32] = sha2::Sha256::digest(value).into();
            debug!("Computed SHA256 hash: {:?}", hash);
            Some(hash)
        }
        PreimageKeyType::Local | PreimageKeyType::GlobalGeneric => {
            debug!("Local/GlobalGeneric key type - no hash validation needed");
            None
        }
        PreimageKeyType::Precompile => {
            debug!("Precompile key type - not supported in zkVM");
            unimplemented!("Precompile not supported in zkVM")
        }
        PreimageKeyType::Blob => {
            debug!("Blob key type - should be validated elsewhere");
            unreachable!("Blob keys validated in blob witness")
        }
    } {
        debug!("Expected hash computed, comparing with key");
        let expected_key = PreimageKey::new(expected_hash, key.key_type());
        debug!("Expected key: {:?}", expected_key);
        debug!("Actual key: {:?}", key);

        if key != &expected_key {
            debug!("Key mismatch! Expected: {:?}, Got: {:?}", expected_key, key);
            return Err(PreimageOracleError::InvalidPreimageKey);
        } else {
            debug!("Key validation passed - hashes match");
        }
    } else {
        debug!("No hash validation required for this key type");
    }

    debug!("check_preimage completed successfully");
    Ok(())
}

#[async_trait]
impl HintWriterClient for PreimageStore {
    async fn write(&self, _hint: &str) -> PreimageOracleResult<()> {
        Ok(())
    }
}

#[async_trait]
impl PreimageOracleClient for PreimageStore {
    async fn get(&self, key: PreimageKey) -> PreimageOracleResult<Vec<u8>> {
        let Some(value) = self.preimage_map.get(&key) else {
            return Err(PreimageOracleError::InvalidPreimageKey);
        };
        Ok(value.clone())
    }

    async fn get_exact(&self, key: PreimageKey, buf: &mut [u8]) -> PreimageOracleResult<()> {
        buf.copy_from_slice(&self.get(key).await?);
        Ok(())
    }
}

impl FlushableCache for PreimageStore {
    fn flush(&self) {}
}
