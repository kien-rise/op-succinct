use std::time::Duration;

use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_client::RpcClient;
use alloy_rpc_types_eth::{TransactionReceipt, TransactionRequest};
use anyhow::Result;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::common::{
    args::{ProposerSignerArgs, SignerArgs},
    primitives::parse_duration,
};

#[derive(Debug)]
pub enum TxManagerRequest {
    Send(TransactionRequest, oneshot::Sender<TransactionReceipt>),
}

impl TxManagerRequest {
    pub fn channel() -> (mpsc::Sender<Self>, mpsc::Receiver<Self>) {
        mpsc::channel(256)
    }
}

#[derive(Debug, clap::Args)]
pub struct TxManagerConfig {
    #[arg(id = "tx-manager.num-confirmations", long = "tx-manager.num-confirmations")]
    pub num_confirmations: Option<u64>,

    #[arg(
        id = "tx-manager.timeout",
        long = "tx-manager.timeout",
        value_parser = parse_duration,
        help = "Transaction confirmation timeout. Examples: 60 (seconds), 2m"
    )]
    pub timeout: Option<Duration>,

    #[command(flatten)]
    pub proposer_signer_args: ProposerSignerArgs,
}

pub struct TxManager {
    config: TxManagerConfig,
    l1_rpc: RpcClient,
}

impl TxManager {
    pub fn new(config: TxManagerConfig, l1_rpc: RpcClient) -> Self {
        Self { config, l1_rpc }
    }

    async fn dispatch(&self, request: TxManagerRequest, l1_provider: impl Provider) -> Result<()> {
        match request {
            TxManagerRequest::Send(transaction_request, done) => {
                let mut pending_tx = l1_provider.send_transaction(transaction_request).await?;
                if let Some(num) = self.config.num_confirmations {
                    pending_tx.set_required_confirmations(num);
                }
                if let Some(dur) = self.config.timeout {
                    pending_tx.set_timeout(Some(dur));
                }
                let receipt = pending_tx.get_receipt().await?;
                // TODO: we need to reset the nonce from l1_provider if any tx fails
                let _ = done.send(receipt);
            }
        }
        Ok(())
    }

    pub async fn start(self, ct: CancellationToken, mut rx: mpsc::Receiver<TxManagerRequest>) {
        // TODO: distinguish proposer and challenger wallets
        let wallet = match SignerArgs::from(self.config.proposer_signer_args.clone()).build_wallet()
        {
            Ok(w) => w,
            Err(err) => {
                tracing::error!(%err, "Failed to build wallet");
                return;
            }
        };
        let l1_provider = ProviderBuilder::new().wallet(wallet).connect_client(self.l1_rpc.clone());

        loop {
            match ct.run_until_cancelled(rx.recv()).await {
                Some(Some(request)) => {
                    tracing::debug!(?request, "Handling request");
                    if let Err(err) = self.dispatch(request, &l1_provider).await {
                        tracing::error!(%err, "Failed to handle request");
                    }
                }
                Some(None) => {
                    tracing::info!("Channel closed, stopping");
                    break;
                }
                None => {
                    tracing::info!("Shutdown signal received, stopping");
                    break;
                }
            }
        }
    }
}
