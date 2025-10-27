use serde::de::{self, Unexpected, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::convert::TryFrom;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum MarketVenue {
    BinanceSpot,
    Deepbook,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MarketSide {
    Bid,
    Ask,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderBookLevel {
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MarketEvent {
    pub venue: MarketVenue,
    pub pair: String,
    pub bids: Vec<OrderBookLevel>,
    pub asks: Vec<OrderBookLevel>,
    pub ts_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderCommand {
    pub venue: MarketVenue,
    pub pair: String,
    pub side: MarketSide,
    pub size: f64,
    pub price: Option<f64>,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NaviAssetId {
    Sui = 0,
    Usdc = 10,
    Wal = 24,
}

impl NaviAssetId {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sui => "sui",
            Self::Usdc => "usdc",
            Self::Wal => "wal",
        }
    }
}

impl Default for NaviAssetId {
    fn default() -> Self {
        Self::Usdc
    }
}

impl fmt::Display for NaviAssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl From<NaviAssetId> for u8 {
    fn from(value: NaviAssetId) -> Self {
        value as u8
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidNaviAssetId {
    value: String,
}

impl InvalidNaviAssetId {
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }

    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for InvalidNaviAssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown Navi asset id: {}", self.value)
    }
}

impl std::error::Error for InvalidNaviAssetId {}

impl TryFrom<u8> for NaviAssetId {
    type Error = InvalidNaviAssetId;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Sui),
            10 => Ok(Self::Usdc),
            24 => Ok(Self::Wal),
            _ => Err(InvalidNaviAssetId::new(value.to_string())),
        }
    }
}

impl FromStr for NaviAssetId {
    type Err = InvalidNaviAssetId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        match trimmed.to_ascii_lowercase().as_str() {
            "sui" => Ok(Self::Sui),
            "usdc" => Ok(Self::Usdc),
            "wal" => Ok(Self::Wal),
            _ => Err(InvalidNaviAssetId::new(trimmed)),
        }
    }
}

impl Serialize for NaviAssetId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for NaviAssetId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct NaviAssetIdVisitor;

        impl<'de> Visitor<'de> for NaviAssetIdVisitor {
            type Value = NaviAssetId;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a Navi asset id (sui, usdc, wal or numeric id)")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                NaviAssetId::from_str(value).map_err(|_| {
                    de::Error::invalid_value(
                        Unexpected::Str(value),
                        &"sui, usdc, wal or numeric id",
                    )
                })
            }

            fn visit_u8<E>(self, value: u8) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                NaviAssetId::try_from(value).map_err(|_| {
                    de::Error::invalid_value(
                        Unexpected::Unsigned(value.into()),
                        &"a supported Navi asset id",
                    )
                })
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value > u8::MAX as u64 {
                    return Err(E::invalid_value(
                        Unexpected::Unsigned(value),
                        &"a u8 Navi asset id",
                    ));
                }
                self.visit_u8(value as u8)
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value < 0 || value > u8::MAX as i64 {
                    return Err(E::invalid_value(
                        Unexpected::Signed(value),
                        &"a non-negative Navi asset id within u8 range",
                    ));
                }
                self.visit_u8(value as u8)
            }
        }

        deserializer.deserialize_any(NaviAssetIdVisitor)
    }
}
