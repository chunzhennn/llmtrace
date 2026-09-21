use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::types::TokenUsage;

pub type PriceTable = BTreeMap<String, ModelPrice>;

/// Deployment-specific text-token estimates, in USD per million tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

impl ModelPrice {
    pub fn valid(&self) -> bool {
        [
            Some(self.input),
            Some(self.output),
            self.cache_read,
            self.cache_write,
        ]
        .into_iter()
        .flatten()
        .all(|rate| rate.is_finite() && (0.0..=1_000_000.0).contains(&rate))
    }

    pub fn estimate_microusd(&self, usage: &TokenUsage) -> Option<i64> {
        let input = usage.input_tokens?;
        let output = usage.output_tokens?;
        let cached = usage.cached_input_tokens.unwrap_or(0);
        let written = usage.cache_creation_input_tokens.unwrap_or(0);
        if !self.valid()
            || [input, output, cached, written]
                .iter()
                .any(|count| *count < 0)
        {
            return None;
        }
        let uncached = input.checked_sub(cached)?.checked_sub(written)?;
        if uncached < 0 {
            return None;
        }
        let cache_read = if cached > 0 { self.cache_read? } else { 0.0 };
        let cache_write = if written > 0 { self.cache_write? } else { 0.0 };
        // USD / million tokens equals micro-USD / token. Round once per request.
        let cost = (uncached as f64 * self.input
            + output as f64 * self.output
            + cached as f64 * cache_read
            + written as f64 * cache_write)
            .round();
        (cost.is_finite() && cost < i64::MAX as f64).then_some(cost as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_prices_cache_reads_separately() {
        let price = ModelPrice {
            input: 2.0,
            output: 8.0,
            cache_read: Some(0.5),
            cache_write: None,
        };
        let usage = TokenUsage {
            input_tokens: Some(1000),
            output_tokens: Some(100),
            cached_input_tokens: Some(800),
            ..Default::default()
        };
        assert_eq!(price.estimate_microusd(&usage), Some(1600));
        assert_eq!(
            ModelPrice {
                cache_read: None,
                ..price.clone()
            }
            .estimate_microusd(&usage),
            None
        );
        assert_eq!(
            price.estimate_microusd(&TokenUsage {
                cached_input_tokens: Some(2000),
                ..usage
            }),
            None
        );
        assert_eq!(price.estimate_microusd(&TokenUsage::default()), None);
    }
}
