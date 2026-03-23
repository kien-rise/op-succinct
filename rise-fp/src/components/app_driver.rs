use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{common::primitives::parse_duration, components::game_fetcher::GameFetcherRequest};

#[derive(Debug, clap::Args)]
pub struct AppDriverConfig {
    #[arg(
        id = "driver.fetch-interval",
        long = "driver.fetch-interval",
        value_parser = parse_duration,
        default_value = "60",
        help = "Fetching interval. Examples: 2.5 (seconds), 1m, 500ms, 10s."
    )]
    pub fetch_interval: Duration,
}

pub struct AppDriver {
    config: AppDriverConfig,
    fetcher_tx: mpsc::Sender<GameFetcherRequest>,
}

impl AppDriver {
    pub fn new(
        config: AppDriverConfig,
        fetcher_tx: mpsc::Sender<GameFetcherRequest>,
    ) -> Self {
        Self { config, fetcher_tx }
    }

    pub async fn start(self, ct: CancellationToken) {
        let mut immediate = true;

        loop {
            let delay = if immediate { Duration::ZERO } else { self.config.fetch_interval };
            if ct.run_until_cancelled(tokio::time::sleep(delay)).await.is_none() {
                tracing::info!("Shutdown signal received, stopping");
                break;
            }

            let (t, r) = oneshot::channel();
            if self.fetcher_tx.send(GameFetcherRequest::Step(t)).await.is_err() {
                tracing::info!("Channel closed, stopping");
                break;
            };

            immediate = if let Ok(has_progress) = r.await { has_progress } else { false }
        }
    }
}
