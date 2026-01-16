//! A program to verify a Optimism L2 block STF with Ethereum DA in the zkVM.
//!
//! This binary contains the client program for executing the Optimism rollup state transition
//! across a range of blocks, which can be used to generate an on chain validity proof. Depending on
//! the compilation pipeline, it will compile to be run either in native mode or in zkVM mode. In
//! native mode, the data for verifying the batch validity is fetched from RPC, while in zkVM mode,
//! the data is supplied by the host binary to the verifiable program.

#![no_main]
sp1_zkvm::entrypoint!(main);

use canoe_sp1_cc_verifier::CanoeSp1CCVerifier;
use canoe_verifier_address_fetcher::CanoeVerifierAddressFetcherDeployedByEigenLabs;
use hokulea_zkvm_verification::eigenda_witness_to_preloaded_provider;
use op_succinct_client_utils::witness::{EigenDAWitnessData, WitnessData};
use op_succinct_eigenda_client_utils::executor::EigenDAWitnessExecutor;
use op_succinct_range_utils::run_range_program;
use rkyv::rancor::Error;

fn main() {
    #[cfg(feature = "tracing-subscriber")]
    op_succinct_range_utils::setup_tracing();

    kona_proof::block_on(async move {
        let witness_rkyv_bytes: Vec<u8> = sp1_zkvm::io::read_vec();
        let mut witness_data = rkyv::from_bytes::<EigenDAWitnessData, Error>(&witness_rkyv_bytes)
            .expect("Failed to deserialize witness data.");
        let eigenda_witness =
            witness_data.eigenda_witness.take().expect("eigenda witness data is not present");

        let (oracle, beacon) = witness_data
            .clone()
            .get_oracle_and_blob_provider()
            .await
            .expect("Failed to load oracle and blob provider");

        let preloaded_preimage_provider = eigenda_witness_to_preloaded_provider(
            oracle.clone(),
            CanoeSp1CCVerifier {},
            CanoeVerifierAddressFetcherDeployedByEigenLabs {},
            eigenda_witness,
        )
        .await
        .expect("Failed to get preloaded blob provider");

        run_range_program(EigenDAWitnessExecutor::new(preloaded_preimage_provider), oracle, beacon)
            .await;
    });
}
