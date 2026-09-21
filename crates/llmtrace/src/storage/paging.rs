//! Pagination and time windows shared by read endpoints.
use super::models::PageInfo;
use chrono::{DateTime, Duration, Utc};

const DEFAULT_HOURS: i64 = 24;
const MAX_HOURS: i64 = 24 * 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Page {
    pub limit: i64,
    pub offset: i64,
}

impl Page {
    pub fn from_query(limit: Option<i64>, offset: Option<i64>) -> Self {
        Self {
            limit: limit.unwrap_or(100).clamp(1, 500),
            offset: offset.unwrap_or(0).clamp(0, 1_000_000),
        }
    }

    pub fn fetch_limit(self) -> i64 {
        self.limit + 1
    }

    pub fn info(self, has_more: bool) -> PageInfo {
        PageInfo {
            limit: self.limit,
            offset: self.offset,
            has_more,
            next_offset: has_more.then_some(self.offset.saturating_add(self.limit)),
        }
    }
}

pub(super) struct WindowSpec {
    default_limit: i64,
    max_limit: i64,
}

pub(super) const TOP_N_WINDOW: WindowSpec = WindowSpec {
    default_limit: 10,
    max_limit: 50,
};
pub(super) const REQUEST_FACET_WINDOW: WindowSpec = WindowSpec {
    default_limit: 25,
    max_limit: 100,
};
pub(super) const RECENT_ERROR_WINDOW: WindowSpec = WindowSpec {
    default_limit: 50,
    max_limit: 200,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Window {
    pub since_hours: i64,
    pub limit: i64,
}

impl WindowSpec {
    pub fn resolve(&self, hours: Option<i64>, limit: Option<i64>) -> Window {
        Window {
            since_hours: bounded_hours(hours, MAX_HOURS),
            limit: limit.unwrap_or(self.default_limit).clamp(1, self.max_limit),
        }
    }
}

impl Window {
    pub fn metadata(self, cutoff: DateTime<Utc>) -> serde_json::Value {
        serde_json::json!({"since_hours": self.since_hours, "started_at_gte": cutoff, "limit": self.limit})
    }

    pub fn cutoff(self, now: DateTime<Utc>) -> DateTime<Utc> {
        now - Duration::hours(self.since_hours)
    }

    pub fn fetch_limit(self) -> i64 {
        self.limit + 1
    }
}

fn bounded_hours(hours: Option<i64>, max: i64) -> i64 {
    hours.unwrap_or(DEFAULT_HOURS).clamp(1, max)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LookbackWindow {
    pub since_hours: i64,
}

impl LookbackWindow {
    pub fn from_query(hours: Option<i64>) -> Self {
        Self {
            since_hours: bounded_hours(hours, MAX_HOURS),
        }
    }

    pub fn cutoff(self, now: DateTime<Utc>) -> DateTime<Utc> {
        now - Duration::hours(self.since_hours)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UsageTimeseriesBucket {
    Minute,
    Hour,
    Day,
}

impl UsageTimeseriesBucket {
    pub fn parse(value: Option<&str>) -> anyhow::Result<Self> {
        match value.unwrap_or("hour").trim().to_ascii_lowercase().as_str() {
            "" | "hour" => Ok(Self::Hour),
            "minute" => Ok(Self::Minute),
            "day" => Ok(Self::Day),
            _ => anyhow::bail!("bucket must be one of minute, hour, or day"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minute => "minute",
            Self::Hour => "hour",
            Self::Day => "day",
        }
    }

    pub fn interval_sql(self) -> &'static str {
        match self {
            Self::Minute => "'1 minute'::interval",
            Self::Hour => "'1 hour'::interval",
            Self::Day => "'1 day'::interval",
        }
    }

    fn max_since_hours(self) -> i64 {
        match self {
            Self::Minute => 24,
            Self::Hour => MAX_HOURS,
            Self::Day => 24 * 365,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UsageTimeseriesWindow {
    pub since_hours: i64,
    pub bucket: UsageTimeseriesBucket,
}

impl UsageTimeseriesWindow {
    pub fn from_query(hours: Option<i64>, bucket: Option<&str>) -> anyhow::Result<Self> {
        let bucket = UsageTimeseriesBucket::parse(bucket)?;
        Ok(Self {
            since_hours: bounded_hours(hours, bucket.max_since_hours()),
            bucket,
        })
    }

    pub fn cutoff(self, now: DateTime<Utc>) -> DateTime<Utc> {
        now - Duration::hours(self.since_hours)
    }
}

#[cfg(test)]
mod tests;
