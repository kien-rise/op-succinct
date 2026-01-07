use std::{collections::HashMap, path::PathBuf, sync::Arc};

use alloy_primitives::{Address, BlockNumber};
use anyhow::Result;
use op_succinct_client_utils::witness::EigenDAWitnessData;
use tokio::sync::{Mutex, OnceCell};

type Witness = EigenDAWitnessData;

async fn generate_witness_slow(
    l2_start_block: BlockNumber,
    l2_end_block: BlockNumber,
) -> Result<Witness> {
    Ok(EigenDAWitnessData::default()) // TODO: implement this
}

struct WitnessPrecacher {
    root_dir: PathBuf,
    cache: Arc<Mutex<HashMap<(BlockNumber, BlockNumber), Arc<OnceCell<PathBuf>>>>>,
}

impl WitnessPrecacher {
    async fn precache_witness(
        &self,
        l2_start_block: BlockNumber,
        l2_end_block: BlockNumber,
    ) -> Result<PathBuf> {
        tracing::info!(%l2_start_block, %l2_end_block, "precache_witness");
        let cell = (self.cache.lock().await)
            .entry((l2_start_block, l2_end_block))
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone();

        let path = cell
            .get_or_try_init::<anyhow::Error, _, _>(|| {
                let path = self.root_dir.join(format!("{l2_start_block}-{l2_end_block}.bin"));
                async {
                    let witness = generate_witness_slow(l2_start_block, l2_end_block).await?;
                    let bytes = serde_cbor::to_vec(&witness)?;
                    tokio::fs::write(path.clone(), bytes).await?;
                    Ok(path)
                }
            })
            .await?
            .clone();
        Ok(path)
    }

    async fn get_witness_or_generate(
        &self,
        l2_start_block: BlockNumber,
        l2_end_block: BlockNumber,
    ) -> Result<Witness> {
        let path = self.precache_witness(l2_start_block, l2_end_block).await?;
        let bytes = tokio::fs::read(path).await?;
        let witness = serde_cbor::from_slice(&bytes)?;
        Ok(witness)
    }
}

#[cfg(all(test, feature = "integration"))]
mod tests {
    use super::*;

    use alloy_primitives::{address, BlockNumber, B256};
    use anyhow::Result;
    use clap::Parser;
    use op_succinct_eigenda_host_utils::{host::EigenDAOPSuccinctHost, witness_generator::EigenDAWitnessGenerator};
    use op_succinct_host_utils::{fetcher::OPSuccinctDataFetcher, host::OPSuccinctHost};
    use op_succinct_proof_utils::initialize_host;
    use std::{ffi::OsString, sync::OnceLock};

    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    /*
    RUST_LOG=trace cargo test --release \
        --package op-succinct-fp --lib --features eigenda --features integration -- \
        witness_precacher::tests::run_generate_witness_slow --exact --nocapture --ignored -- \
        "$L2_START_BLOCK" "$L2_END_BLOCK"
    */
    async fn run_generate_witness_slow() -> Result<()> {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .init();
        println!("{:?}", std::env::current_dir());
        #[derive(Parser, Debug)]
        struct Args {
            l2_start_block: BlockNumber,
            l2_end_block: BlockNumber,
        }

        let args = Args::parse_from(std::env::args_os().skip_while(|arg| arg != "--"));
        tracing::info!(?args, "args");

        let fetcher = OPSuccinctDataFetcher::new_with_rollup_config().await?;
        let host = EigenDAOPSuccinctHost {
            fetcher: Arc::new(fetcher.clone()),
            witness_generator: Arc::new(EigenDAWitnessGenerator::new(
                fetcher.rpc_config.l1_rpc_client(),
                true,
            )),
        };
        let host_args = host.fetch(args.l2_start_block, args.l2_end_block, None, false).await?;
        let witness_data = host.run(&host_args).await?;
        tracing::info!(?witness_data, "witness_data");
        Ok(())
    }

    // #[tokio::test]
    // #[ignore]
    // async fn test_manually_precache_witness() -> Result<()> {
    //     tracing_subscriber::fmt()
    //         .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
    //         .init();

    //     #[derive(Parser, Debug)]
    //     struct Args {
    //         l2_start_block: BlockNumber,
    //         l2_end_block: BlockNumber,
    //     }

    //     let args = Args::parse_from(std::env::args_os().skip_while(|arg| arg != "--"));
    //     tracing::info!(?args, "args");

    //     let fetcher = OPSuccinctDataFetcher::new_with_rollup_config().await?;
    //     let host = initialize_host(Arc::new(fetcher.clone()));

    //     let temp_dir = tempfile::tempdir()?;
    //     let precacher = WitnessPrecacher {
    //         root_dir: temp_dir.path().to_path_buf(),
    //         cache: Arc::new(Mutex::new(HashMap::new())),
    //     };

    //     let game_address = address!("0x1234567890123456789012345678901234567890");
    //     let path = precacher.precache_witness(game_address).await.unwrap();

    //     // Verify the file was created
    //     assert!(path.exists());
    //     assert_eq!(path.file_name().unwrap(), PathBuf::from(format!("{}.bin", game_address)));

    //     Ok(())
    // }
}
