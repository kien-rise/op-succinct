use alloy_consensus::BlockBody;
use alloy_primitives::B256;
use alloy_rlp::Decodable;
use anyhow::Result;
use kona_derive::{Pipeline, PipelineError, PipelineErrorKind, Signal, SignalReceiver};
use kona_driver::{Driver, DriverError, DriverPipeline, DriverResult, Executor, TipCursor};
use kona_genesis::RollupConfig;
use kona_preimage::{CommsClient, PreimageKey};
use kona_proof::{errors::OracleProviderError, HintType};
use kona_protocol::L2BlockInfo;
use op_alloy_consensus::{OpBlock, OpTxEnvelope, OpTxType};
use std::fmt::Debug;
use tracing::{error, info, warn};

/// Fetches the safe head hash of the L2 chain based on the agreed upon L2 output root in the
/// [BootInfo].
pub(crate) async fn fetch_safe_head_hash<O>(
    caching_oracle: &O,
    agreed_l2_output_root: B256,
) -> Result<B256, OracleProviderError>
where
    O: CommsClient,
{
    let mut output_preimage = [0u8; 128];
    HintType::StartingL2Output
        .with_data(&[agreed_l2_output_root.as_ref()])
        .send(caching_oracle)
        .await?;
    caching_oracle
        .get_exact(PreimageKey::new_keccak256(*agreed_l2_output_root), output_preimage.as_mut())
        .await?;

    output_preimage[96..128].try_into().map_err(OracleProviderError::SliceConversion)
}

// Sourced from kona/crates/driver/src/core.rs with modifications to use the L2 provider's caching
// system. After each block execution, we update the L2 provider's caches (header_by_number,
// block_by_number, system_config_by_number, l2_block_info_by_number) with the new block data. This
// ensures subsequent lookups for this block number can be served directly from cache rather than
// requiring oracle queries.
/// Advances the derivation pipeline to the target block number.
///
/// ## Takes
/// - `cfg`: The rollup configuration.
/// - `target`: The target block number.
///
/// ## Returns
/// - `Ok((l2_safe_head, output_root))` - A tuple containing the [L2BlockInfo] of the produced block
///   and the output root.
/// - `Err(e)` - An error if the block could not be produced.
pub async fn advance_to_target<E, DP, P>(
    driver: &mut Driver<E, DP, P>,
    cfg: &RollupConfig,
    mut target: Option<u64>,
) -> DriverResult<(L2BlockInfo, B256), E::Error>
where
    E: Executor + Send + Sync + Debug,
    DP: DriverPipeline<P> + Send + Sync + Debug,
    P: Pipeline + SignalReceiver + Send + Sync + Debug,
{
    info!(target: "client", "SC: Starting advance_to_target with target: {:?}", target);
    info!("SC: Starting advance_to_target with target: {:?}", target);

    loop {
        // Check if we have reached the target block number.
        let pipeline_cursor = driver.cursor.read();
        let tip_cursor = pipeline_cursor.tip();
        tracing::debug!("SC: Current L2 safe head block number: {}", tip_cursor.l2_safe_head.block_info.number);

        if let Some(tb) = target {
            tracing::debug!("SC: Checking if current block {} >= target block {}", tip_cursor.l2_safe_head.block_info.number, tb);
            if tip_cursor.l2_safe_head.block_info.number >= tb {
                info!(target: "client", "SC: Derivation complete, reached L2 safe head. Current: {}, Target: {}", tip_cursor.l2_safe_head.block_info.number, tb);
                return Ok((tip_cursor.l2_safe_head, tip_cursor.l2_safe_head_output_root));
            }
        } else {
            tracing::debug!("SC: No target specified, continuing derivation");
        }

        #[cfg(target_os = "zkvm")]
        println!("cycle-tracker-report-start: payload-derivation");
        tracing::debug!("SC: Producing payload for L2 safe head: {}", tip_cursor.l2_safe_head.block_info.number);
        let mut attributes = match driver.pipeline.produce_payload(tip_cursor.l2_safe_head).await {
            Ok(attrs) => {
                tracing::debug!("SC: Successfully produced payload attributes");
                attrs.take_inner()
            },
            Err(PipelineErrorKind::Critical(PipelineError::EndOfSource)) => {
                warn!(target: "client", "SC: Exhausted data source; Halting derivation and using current safe head.");

                // Adjust the target block number to the current safe head, as no more blocks
                // can be produced.
                if target.is_some() {
                    info!(target: "client", "SC: Adjusting target from {:?} to current safe head: {}", target, tip_cursor.l2_safe_head.block_info.number);
                    target = Some(tip_cursor.l2_safe_head.block_info.number);
                };

                // If we are in interop mode, this error must be handled by the caller.
                // Otherwise, we continue the loop to halt derivation on the next iteration.
                let current_block = driver.cursor.read().l2_safe_head().block_info.number;
                let is_interop_active = cfg.is_interop_active(current_block);
                tracing::debug!("SC: Interop active for block {}: {}", current_block, is_interop_active);
                if is_interop_active {
                    error!(target: "client", "SC: EndOfSource error in interop mode, returning error");
                    return Err(PipelineError::EndOfSource.crit().into());
                } else {
                    tracing::debug!("SC: Continuing loop to halt derivation");
                    continue;
                }
            }
            Err(e) => {
                error!(target: "client", "SC: Failed to produce payload: {:?}", e);
                return Err(DriverError::Pipeline(e));
            }
        };
        #[cfg(target_os = "zkvm")]
        println!("cycle-tracker-report-end: payload-derivation");

        tracing::debug!("SC: Updating executor safe head to: {:?}", tip_cursor.l2_safe_head_header.hash_slow());
        driver.executor.update_safe_head(tip_cursor.l2_safe_head_header.clone());

        #[cfg(target_os = "zkvm")]
        println!("cycle-tracker-report-start: block-execution");
        tracing::debug!("SC: Executing payload with timestamp: {}", attributes.payload_attributes.timestamp);
        let outcome = match driver.executor.execute_payload(attributes.clone()).await {
            Ok(outcome) => {
                tracing::debug!("SC: Successfully executed payload, outcome header: {:?}", outcome.header);
                outcome
            },
            Err(e) => {
                error!(target: "client", "SC: Failed to execute L2 block: {}", e);

                let is_holocene_active = cfg.is_holocene_active(attributes.payload_attributes.timestamp);
                tracing::debug!("SC: Holocene active for timestamp {}: {}", attributes.payload_attributes.timestamp, is_holocene_active);

                if is_holocene_active {
                    // Retry with a deposit-only block.
                    warn!(target: "client", "SC: Flushing current channel and retrying deposit only block");

                    // Flush the current batch and channel - if a block was replaced with a
                    // deposit-only block due to execution failure, the
                    // batch and channel it is contained in is forwards
                    // invalidated.
                    tracing::debug!("SC: Sending FlushChannel signal to pipeline");
                    driver.pipeline.signal(Signal::FlushChannel).await?;

                    // Strip out all transactions that are not deposits.
                    let original_tx_count = attributes.transactions.as_ref().map(|txs| txs.len()).unwrap_or(0);
                    attributes.transactions = attributes.transactions.map(|txs| {
                        txs.into_iter()
                            .filter(|tx| !tx.is_empty() && tx[0] == OpTxType::Deposit as u8)
                            .collect::<Vec<_>>()
                    });
                    let deposit_tx_count = attributes.transactions.as_ref().map(|txs| txs.len()).unwrap_or(0);
                    tracing::debug!("SC: Filtered transactions from {} to {} (deposits only)", original_tx_count, deposit_tx_count);

                    // Retry the execution.
                    tracing::debug!("SC: Retrying execution with deposit-only block");
                    driver.executor.update_safe_head(tip_cursor.l2_safe_head_header.clone());
                    match driver.executor.execute_payload(attributes.clone()).await {
                        Ok(header) => {
                            tracing::debug!("SC: Successfully executed deposit-only block");
                            header
                        },
                        Err(e) => {
                            error!(
                                target: "client",
                                "SC: Critical - Failed to execute deposit-only block: {e}",
                            );
                            return Err(DriverError::Executor(e));
                        }
                    }
                } else {
                    // Pre-Holocene, discard the block if execution fails.
                    tracing::debug!("SC: Pre-Holocene mode: discarding failed block and continuing");
                    continue;
                }
            }
        };
        #[cfg(target_os = "zkvm")]
        println!("cycle-tracker-report-end: block-execution");

        // Construct the block.
        tracing::debug!("SC: Constructing OpBlock from execution outcome");
        let tx_count = attributes.transactions.as_ref().unwrap_or(&Vec::new()).len();
        tracing::debug!("SC: Block will contain {} transactions", tx_count);

        let block = OpBlock {
            header: outcome.header.inner().clone(),
            body: BlockBody {
                transactions: attributes
                    .transactions
                    .as_ref()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .map(|tx| OpTxEnvelope::decode(&mut tx.as_ref()).map_err(DriverError::Rlp))
                    .collect::<DriverResult<Vec<OpTxEnvelope>, E::Error>>()?,
                ommers: Vec::new(),
                withdrawals: None,
            },
        };
        tracing::debug!("SC: Successfully constructed block with hash: {:?}", block.header.hash_slow());

        // Get the pipeline origin and update the tip cursor.
        tracing::debug!("SC: Getting pipeline origin and creating L2BlockInfo");
        let origin = driver.pipeline.origin().ok_or(PipelineError::MissingOrigin.crit())?;
        tracing::debug!("SC: Pipeline origin: {:?}", origin);

        let l2_info =
            L2BlockInfo::from_block_and_genesis(&block, &driver.pipeline.rollup_config().genesis)?;
        tracing::debug!("SC: Created L2BlockInfo: block_number={}, block_hash={:?}", l2_info.block_info.number, l2_info.block_info.hash);

        tracing::debug!("SC: Computing output root");
        let output_root = driver.executor.compute_output_root().map_err(DriverError::Executor)?;
        tracing::debug!("SC: Computed output root: {:?}", output_root);

        let tip_cursor = TipCursor::new(
            l2_info,
            outcome.header,
            output_root,
        );
        tracing::debug!("SC: Created new tip cursor for block: {}", tip_cursor.l2_safe_head.block_info.number);

        // Advance the derivation pipeline cursor
        tracing::debug!("SC: Advancing derivation pipeline cursor");
        drop(pipeline_cursor);
        driver.cursor.write().advance(origin, tip_cursor);
        tracing::debug!("SC: Successfully advanced cursor, completing iteration");

        // Add forget calls to save cycles
        #[cfg(target_os = "zkvm")]
        std::mem::forget(block);
    }
}
