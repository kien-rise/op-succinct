//! A program to verify a Optimism L2 block STF with Ethereum DA in the zkVM.
//!
//! This binary contains the client program for executing the Optimism rollup state transition
//! across a range of blocks, which can be used to generate an on chain validity proof. Depending on
//! the compilation pipeline, it will compile to be run either in native mode or in zkVM mode. In
//! native mode, the data for verifying the batch validity is fetched from RPC, while in zkVM mode,
//! the data is supplied by the host binary to the verifiable program.

#![no_main]
sp1_zkvm::entrypoint!(main);

use alloy_primitives::{Address, B256};
use canoe_sp1_cc_verifier::CanoeSp1CCVerifier;
use canoe_verifier_address_fetcher::CanoeVerifierAddressFetcherDeployedByEigenLabs;
use hokulea_proof::eigenda_witness::EigenDAWitness;
use hokulea_zkvm_verification::eigenda_witness_to_preloaded_provider;
use op_succinct_client_utils::witness::{EigenDAWitnessData, WitnessData};
use op_succinct_eigenda_client_utils::executor::EigenDAWitnessExecutor;
use op_succinct_range_utils::run_range_program;
#[cfg(feature = "tracing-subscriber")]
use op_succinct_range_utils::setup_tracing;
use rkyv::rancor::Error;

// stolen from ~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/sp1-prover-5.2.2/src/utils.rs
fn bytes_to_words_be(bytes: &[u8; 32]) -> [u32; 8] {
    let mut words = [0u32; 8];
    for i in 0..8 {
        let chunk: [u8; 4] = bytes[i * 4..(i + 1) * 4].try_into().unwrap();
        words[i] = u32::from_be_bytes(chunk);
    }
    words
}

fn main() {
    #[cfg(feature = "tracing-subscriber")]
    setup_tracing();

    kona_proof::block_on(async move {
        let witness_rkyv_bytes: Vec<u8> = sp1_zkvm::io::read_vec();
        let witness_data = rkyv::from_bytes::<EigenDAWitnessData, Error>(&witness_rkyv_bytes)
            .expect("Failed to deserialize witness data.");

        let (oracle, beacon) = witness_data
            .clone()
            .get_oracle_and_blob_provider()
            .await
            .expect("Failed to load oracle and blob provider");

        let eigenda_witness: EigenDAWitness = serde_cbor::from_slice(
            &witness_data.eigenda_data.clone().expect("eigenda witness data is not present"),
        )
        .expect("cannot deserialize eigenda witness");
        let canoe_sp1_cc_key = match option_env!("CANOE_VERIFIER_VKEY") {
            Some(vkey_hex) => {
                let vkey_bytes = vkey_hex
                    .parse::<B256>()
                    .expect("CANOE_VERIFIER_VKEY must be a valid hex string");
                CanoeSp1CCVerifier::new(bytes_to_words_be(&vkey_bytes))
            }
            None => CanoeSp1CCVerifier::default(),
        };
        let preloaded_preimage_provider =
            if let Some(addr_str) = option_env!("CANOE_VERIFIER_ADDRESS") {
                eigenda_witness_to_preloaded_provider(
                    oracle.clone(),
                    canoe_sp1_cc_key,
                    addr_str
                        .parse::<Address>()
                        .expect("CANOE_VERIFIER_ADDRESS must be a valid hex address"),
                    eigenda_witness,
                )
                .await
            } else {
                eigenda_witness_to_preloaded_provider(
                    oracle.clone(),
                    canoe_sp1_cc_key,
                    CanoeVerifierAddressFetcherDeployedByEigenLabs {},
                    eigenda_witness,
                )
                .await
            }
            .expect("Failed to get preloaded blob provider");

        run_range_program(EigenDAWitnessExecutor::new(preloaded_preimage_provider), oracle, beacon)
            .await;
    });
}
