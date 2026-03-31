use std::{
    fmt::{Debug, Display},
    num::{ParseFloatError, ParseIntError},
    ops::{Bound, Range, RangeBounds},
    str::FromStr,
    time::Duration,
};

use alloy_primitives::{Address, BlockNumber, BlockTimestamp, B256};
use alloy_rpc_client::{ClientBuilder, RpcClient};
use alloy_transport::layers::ThrottleLayer;
use alloy_transport_http::reqwest::Url;

use crate::common::contract::{ClaimData, GameStatus, ProposalStatus};

pub type GameIndex = u32;

pub struct GameRangeInclusive {
    start: Option<GameIndex>,
    end: Option<GameIndex>,
}

impl GameRangeInclusive {
    pub fn new(start: Option<GameIndex>, end: Option<GameIndex>) -> Self {
        Self { start, end }
    }

    pub fn clamp(&self, mut game_index: GameIndex) -> GameIndex {
        if let Some(i) = self.end {
            game_index = game_index.min(i)
        }
        if let Some(i) = self.start {
            game_index = game_index.max(i);
        }
        game_index
    }
}

impl RangeBounds<GameIndex> for GameRangeInclusive {
    fn start_bound(&self) -> Bound<&GameIndex> {
        self.start.as_ref().map_or(Bound::Unbounded, Bound::Included)
    }

    fn end_bound(&self) -> Bound<&GameIndex> {
        self.end.as_ref().map_or(Bound::Unbounded, Bound::Included)
    }
}

pub fn pick_random_games(range: Range<GameIndex>, len: usize) -> Vec<GameIndex> {
    if len == 0 || range.is_empty() {
        return vec![]
    }

    if len >= range.len() {
        return range.collect();
    }

    rand::seq::index::sample(&mut rand::rng(), range.len(), len)
        .into_iter()
        .map(|i| range.start + i as GameIndex)
        .collect()
}

#[derive(Debug, PartialEq)]
pub struct GameSnapshot {
    pub game_type: u32,
    pub created_at: BlockTimestamp,
    pub game_address: Address,

    // IDisputeGame
    pub output_root: B256,
    pub claim_block: BlockNumber,
    pub game_status: GameStatus, // mutable
    pub is_blacklisted: bool,    // mutable

    // OPSuccinctFaultDisputeGame
    pub start_block: BlockNumber,
    pub parent_index: GameIndex,
    pub claim_data: ClaimData, // mutable
}

impl GameSnapshot {
    // A game is considered "still good" if there exists a possibility that
    // `isGameClaimValid(game)` returns true.
    pub fn still_good(&self, retirement_timestamp: BlockTimestamp) -> bool {
        // TODO: isGameRegistered(game)
        // TODO: isGameRespected(game)
        if self.is_blacklisted {
            return false;
        }
        if self.created_at <= retirement_timestamp {
            return false;
        }
        if self.game_status == GameStatus::CHALLENGER_WINS {
            return false;
        }
        true
    }

    pub fn is_retired(&self, retirement_timestamp: BlockTimestamp) -> bool {
        self.created_at <= retirement_timestamp
    }

    // See contracts/src/fp/OPSuccinctFaultDisputeGame.sol#gameOver()
    pub fn is_over(&self, now: BlockTimestamp) -> bool {
        self.claim_data.deadline < now || self.claim_data.prover != Address::ZERO
    }

    // Check whether calling the challenge() function would succeed.
    // Just because "we can" does not mean "we should".
    // See contracts/src/fp/OPSuccinctFaultDisputeGame.sol#challenge()
    pub fn can_challenge(&self, now: BlockTimestamp) -> bool {
        if self.game_status != GameStatus::IN_PROGRESS {
            return false;
        }
        if self.claim_data.status != ProposalStatus::Unchallenged {
            return false;
        }
        if self.is_over(now) {
            return false;
        }
        true
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnchorRootSnapshot {
    pub output_root: B256,
    pub claim_block: BlockNumber,
    pub anchor_game: Address,
}

impl AnchorRootSnapshot {
    pub fn is_empty(&self) -> bool {
        self.output_root.is_empty() && self.claim_block == 0 && self.anchor_game.is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ParseDurationError {
    #[error("invalid unit: {0}")]
    InvalidUnit(String),
    #[error("failed to parse int")]
    ParseIntError(#[from] ParseIntError),
    #[error("failed to parse float")]
    ParseFloatError(#[from] ParseFloatError),
    #[error("missing amount")]
    MissingAmount,
}

pub fn parse_duration(s: &str) -> Result<Duration, ParseDurationError> {
    let (prefix, suffix) = match s.rfind(|c: char| c.is_ascii_digit()) {
        Some(n) => s.split_at(n + 1),
        None => return Err(ParseDurationError::MissingAmount),
    };

    if suffix.is_empty() {
        return Ok(Duration::from_secs_f64(s.parse()?))
    }

    let amount: u64 = prefix.parse()?;

    let duration = match suffix {
        "h" => Duration::from_hours(amount),
        "m" => Duration::from_mins(amount),
        "s" => Duration::from_secs(amount),
        "ms" => Duration::from_millis(amount),
        "us" => Duration::from_micros(amount),
        "ns" => Duration::from_nanos(amount),
        _ => return Err(ParseDurationError::InvalidUnit(suffix.to_string())),
    };

    Ok(duration)
}

#[derive(Debug, thiserror::Error)]
pub enum ParseRangeError<T: FromStr<Err: Debug + Display>> {
    #[error("missing range separator '..'")]
    MissingSeparator,

    #[error("invalid start value: {0}")]
    InvalidStart(T::Err),

    #[error("invalid end value: {0}")]
    InvalidEnd(T::Err),
}

pub fn parse_range<T: FromStr<Err: Debug + Display>>(
    s: &str,
) -> Result<Range<T>, ParseRangeError<T>> {
    let (start_str, end_str) = s.split_once("..").ok_or(ParseRangeError::MissingSeparator)?;
    let start = start_str.parse::<T>().map_err(ParseRangeError::InvalidStart)?;
    let end = end_str.parse::<T>().map_err(ParseRangeError::InvalidEnd)?;
    Ok(start..end)
}

pub struct RpcArgs {
    pub rpc_url: Url,
    pub max_rps: Option<u32>,
}

impl From<RpcArgs> for RpcClient {
    fn from(args: RpcArgs) -> Self {
        match args.max_rps {
            Some(rps) => ClientBuilder::default().layer(ThrottleLayer::new(rps)).http(args.rpc_url),
            None => ClientBuilder::default().http(args.rpc_url),
        }
    }
}

#[macro_export]
macro_rules! derive_from {
    ($from:ident, $to:ident, [$($field:ident),*]) => {
        impl From<$from> for $to {
            fn from(source: $from) -> Self {
                Self {
                    $(
                        $field: source.$field,
                    )*
                }
            }
        }
    };

    (&$from:ident, $to:ident, [$($field:ident),*]) => {
        impl From<&$from> for $to {
            fn from(source: &$from) -> Self {
                Self {
                    $(
                        $field: source.$field.clone(),
                    )*
                }
            }
        }
    };
}

pub use derive_from;
