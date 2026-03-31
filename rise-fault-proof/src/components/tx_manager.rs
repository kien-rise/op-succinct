use std::time::Duration;

use alloy_network::EthereumWallet;
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_client::RpcClient;
use alloy_rpc_types_eth::{TransactionReceipt, TransactionRequest};
use anyhow::{anyhow, Context, Result};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::common::{
    args::{ChallengerSignerArgs, ProposerSignerArgs, SignerArgs},
    primitives::parse_duration,
};

#[derive(Debug)]
pub enum TxManagerSenderRole {
    Proposer,
    Challenger,
}

#[derive(Debug)]
pub enum TxManagerRequest {
    Send(TxManagerSenderRole, TransactionRequest, oneshot::Sender<TransactionReceipt>),
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
    pub proposer_signer_args: Option<ProposerSignerArgs>,
    #[command(flatten)]
    pub challenger_signer_args: Option<ChallengerSignerArgs>,
}

pub struct TxManager {
    config: TxManagerConfig,
    l1_rpc: RpcClient,
    proposer_wallet: Option<EthereumWallet>,
    challenger_wallet: Option<EthereumWallet>,
}

impl TxManager {
    pub fn new(config: TxManagerConfig, l1_rpc: RpcClient) -> Result<Self> {
        let proposer_wallet = match config.proposer_signer_args.as_ref() {
            Some(args) => Some(SignerArgs::from(args).build_wallet().context("proposer wallet")?),
            None => None,
        };

        let challenger_wallet = match config.challenger_signer_args.as_ref() {
            Some(args) => Some(SignerArgs::from(args).build_wallet().context("challenger wallet")?),
            None => None,
        };

        Ok(Self { config, l1_rpc, proposer_wallet, challenger_wallet })
    }

    async fn process<P: Provider>(
        &self,
        request: TxManagerRequest,
        proposer: Option<&P>,
        challenger: Option<&P>,
    ) -> Result<()> {
        match request {
            TxManagerRequest::Send(sender_role, transaction_request, done) => {
                let sender = match sender_role {
                    TxManagerSenderRole::Proposer => {
                        proposer.ok_or_else(|| anyhow!("proposer wallet not configured"))?
                    }
                    TxManagerSenderRole::Challenger => {
                        challenger.ok_or_else(|| anyhow!("challenger wallet not configured"))?
                    }
                };

                tracing::info!(?sender_role, ?transaction_request, "Broadcasting transaction");
                let mut pending_tx = sender.send_transaction(transaction_request).await?;
                if let Some(num) = self.config.num_confirmations {
                    pending_tx.set_required_confirmations(num);
                }
                if let Some(dur) = self.config.timeout {
                    pending_tx.set_timeout(Some(dur));
                }

                tracing::info!(tx_hash = ?pending_tx.tx_hash(), "Awaiting confirmation");
                let receipt = pending_tx.get_receipt().await?;
                // TODO: we need to reset the nonce from l1_provider if any tx fails

                tracing::info!(tx_hash = ?receipt.transaction_hash, ?receipt, "Submitted transaction successfully");
                let _ = done.send(receipt);
            }
        }
        Ok(())
    }

    pub async fn start(self, ct: CancellationToken, mut rx: mpsc::Receiver<TxManagerRequest>) {
        let proposer: Option<_> = (self.proposer_wallet.as_ref())
            .map(|w| ProviderBuilder::new().wallet(w).connect_client(self.l1_rpc.clone()));

        let challenger: Option<_> = (self.challenger_wallet.as_ref())
            .map(|w| ProviderBuilder::new().wallet(w).connect_client(self.l1_rpc.clone()));

        loop {
            match ct.run_until_cancelled(rx.recv()).await {
                Some(Some(request)) => {
                    tracing::debug!(?request, "Handling request");
                    if let Err(err) =
                        self.process(request, proposer.as_ref(), challenger.as_ref()).await
                    {
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
