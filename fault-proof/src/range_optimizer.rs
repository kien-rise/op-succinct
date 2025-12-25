use std::sync::Arc;

use alloy_primitives::BlockNumber;
use anyhow::Result;
use hokulea_eigenda::{EigenDADataSource, EigenDAPreimageSource};
use kona_derive::{
    ChainProvider, EthereumDataSource, OriginProvider, Pipeline, PipelineBuilder, PipelineError,
    PipelineErrorKind, ResetSignal, SignalReceiver, StatefulAttributesBuilder, StepResult,
};
use kona_genesis::L1ChainConfig;
use kona_protocol::BatchValidationProvider;
use kona_providers_alloy::{
    AlloyChainProvider, AlloyL2ChainProvider, OnlineBeaconClient, OnlineBlobProvider,
};
use kona_registry::RollupConfig;

use crate::eigenda_provider::OnlineEigenDAPreimageProvider;

pub async fn get_target_l2_block_number(
    rollup_config: Arc<RollupConfig>,
    l1_config: Arc<L1ChainConfig>,
    eigenda_preimage_provider: OnlineEigenDAPreimageProvider,
    blob_provider: OnlineBlobProvider<OnlineBeaconClient>,
    mut l1_chain_provider: AlloyChainProvider,
    mut l2_chain_provider: AlloyL2ChainProvider,
    mut l2_block_number: BlockNumber,
) -> Result<BlockNumber> {
    tracing::debug!("Starting range optimization from L2 block {}", l2_block_number);

    let starting_l2_safe_head = l2_chain_provider.l2_block_info_by_number(l2_block_number).await?;
    tracing::debug!("L2 safe head l1_origin: {:?}", starting_l2_safe_head.l1_origin);

    let l1_origin =
        l1_chain_provider.block_info_by_number(starting_l2_safe_head.l1_origin.number).await?;
    tracing::debug!("L1 origin block: {:?}", l1_origin);

    tracing::debug!("Creating data sources (Ethereum + EigenDA)");
    let ethereum_data_source = EthereumDataSource::new_from_parts(
        l1_chain_provider.clone(),
        blob_provider,
        &rollup_config,
    );
    let eigenda_data_source = EigenDAPreimageSource::new(eigenda_preimage_provider);
    let da_provider = EigenDADataSource::new(ethereum_data_source, eigenda_data_source);

    tracing::debug!("Creating StatefulAttributesBuilder");
    let attributes = StatefulAttributesBuilder::new(
        Arc::clone(&rollup_config),
        l1_config.clone(),
        l2_chain_provider.clone(),
        l1_chain_provider.clone(),
    );

    tracing::debug!("Building pipeline with L1 origin block {}", l1_origin.number);
    let mut pipeline = PipelineBuilder::new()
        .rollup_config(rollup_config)
        .dap_source(da_provider)
        .l2_chain_provider(l2_chain_provider.clone())
        .chain_provider(l1_chain_provider.clone())
        .builder(attributes)
        .origin(l1_origin.clone())
        .build_polled();

    {
        let l2_safe_head = l2_chain_provider.l2_block_info_by_number(l2_block_number).await?;
        let l1_origin = pipeline.origin().ok_or(PipelineError::MissingOrigin.crit())?;
        let system_config =
            pipeline.system_config_by_number(l2_safe_head.block_info.number).await?;
        let reset_signal =
            ResetSignal { l2_safe_head, l1_origin, system_config: Some(system_config) };
        pipeline.signal(reset_signal.signal()).await?;
    }

    let mut accumulated_attrs = Vec::new();
    tracing::debug!("Starting pipeline loop");

    loop {
        let l2_safe_head = l2_chain_provider.l2_block_info_by_number(l2_block_number).await?;
        tracing::debug!("Pipeline step for L2 block: {:?}", l2_safe_head.block_info);

        match Pipeline::step(&mut pipeline, l2_safe_head).await {
            StepResult::PreparedAttributes => {
                if let Some(attr) = pipeline.next() {
                    tracing::debug!("PreparedAttributes: L2 {}", l2_block_number + 1);
                    accumulated_attrs.push(attr);
                    l2_block_number += 1;
                }
            }
            StepResult::AdvancedOrigin => {
                tracing::debug!("AdvancedOrigin: attrs={}", accumulated_attrs.len());
                if !accumulated_attrs.is_empty() {
                    tracing::debug!("Breaking loop with {} attrs", accumulated_attrs.len());
                    break;
                }
            }
            StepResult::OriginAdvanceErr(e) | StepResult::StepFailed(e) => match e {
                PipelineErrorKind::Temporary(ref err) => {
                    tracing::debug!("Temporary error, retrying: {:?}", err);
                    continue;
                }
                PipelineErrorKind::Reset(ref err) => {
                    tracing::debug!("Reset error, signaling pipeline reset: {:?}", err);
                    let l1_origin = pipeline.origin().ok_or(PipelineError::MissingOrigin.crit())?;
                    let system_config =
                        pipeline.system_config_by_number(l2_safe_head.block_info.number).await?;
                    let reset_signal =
                        ResetSignal { l2_safe_head, l1_origin, system_config: Some(system_config) };
                    pipeline.signal(reset_signal.signal()).await?;
                    tracing::debug!("Pipeline reset completed");
                }
                PipelineErrorKind::Critical(ref err) => {
                    tracing::error!("Critical pipeline error: {:?}", err);
                    return Err(e.into());
                }
            },
        };
    }

    tracing::debug!("Range optimization complete: {} attrs", accumulated_attrs.len());

    Ok(l2_block_number)
}
