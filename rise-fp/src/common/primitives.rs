use std::{
    fmt::{Debug, Display},
    num::{ParseFloatError, ParseIntError},
    ops::Range,
    str::FromStr,
    time::Duration,
};

use alloy_primitives::{Address, BlockNumber, BlockTimestamp, B256};
use fault_proof::contract::GameStatus;

pub type GameIndex = u32;

#[derive(Debug, PartialEq)]
pub struct GameSnapshot {
    pub proxy_address: Address,
    pub output_root: B256,
    pub claim_block: BlockNumber,
    pub created_at: BlockTimestamp,
    pub start_block: BlockNumber, // specific
    pub parent_index: GameIndex,  // specific
    pub game_status: GameStatus,  // mutable
    pub is_blacklisted: bool,     // mutable
}

impl GameSnapshot {
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
        return true;
    }

    pub fn is_retired(&self, retirement_timestamp: BlockTimestamp) -> bool {
        self.created_at <= retirement_timestamp
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
