//! What a client spent: one entry per answered call.

use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CallUsage {
    /// `None` when the provider does not say.
    pub input_tokens: Option<u64>,
    /// The part of `input_tokens` read back from the provider's prompt cache (billed far lower).
    pub cache_read_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub ms: u64,
}

/// The calls of one client, kept behind a lock because `complete` takes `&self`.
#[derive(Debug, Default)]
pub struct Meter(Mutex<Vec<CallUsage>>);

impl Meter {
    pub fn record(&self, started: Instant, input_tokens: Option<u64>, output_tokens: Option<u64>) {
        self.record_cached(started, input_tokens, None, output_tokens);
    }

    pub fn record_cached(
        &self,
        started: Instant,
        input_tokens: Option<u64>,
        cache_read_tokens: Option<u64>,
        output_tokens: Option<u64>,
    ) {
        let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(CallUsage {
                input_tokens,
                cache_read_tokens,
                output_tokens,
                ms,
            });
    }

    pub fn calls(&self) -> Vec<CallUsage> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Totals and every call, for a `meta.json`; a token total is null when a call did not say.
pub fn usage_json(calls: &[CallUsage]) -> Value {
    let input: Option<u64> = calls.iter().map(|c| c.input_tokens).sum();
    let output: Option<u64> = calls.iter().map(|c| c.output_tokens).sum();
    let cached: Option<u64> = calls.iter().map(|c| c.cache_read_tokens).sum();
    json!({
        "calls": calls.len(),
        "input_tokens": input,
        "cache_read_tokens": cached,
        "output_tokens": output,
        "ms": calls.iter().map(|c| c.ms).sum::<u64>(),
        "per_call": calls
            .iter()
            .map(|c| json!({"input_tokens": c.input_tokens, "cache_read_tokens": c.cache_read_tokens, "output_tokens": c.output_tokens, "ms": c.ms}))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_are_null_when_one_call_has_no_count() {
        let known = CallUsage {
            input_tokens: Some(10),
            cache_read_tokens: None,
            output_tokens: Some(2),
            ms: 5,
        };
        let all = usage_json(&[known, known]);
        assert_eq!(
            (
                all["calls"].clone(),
                all["input_tokens"].clone(),
                all["ms"].clone()
            ),
            (json!(2), json!(20), json!(10))
        );
        let unknown = CallUsage {
            input_tokens: None,
            ..known
        };
        assert_eq!(usage_json(&[known, unknown])["input_tokens"], Value::Null);
    }
}
