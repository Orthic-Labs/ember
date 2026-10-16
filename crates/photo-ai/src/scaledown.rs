//! Native-only ScaleDown Extract adapter (text-only: no image input). Run on a worker/CLI thread,
//! never the UI owner. Only the typed search text and today's date leave the process.
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use lightcraft_fetch::http::{Url, post_json};

use crate::network::{echoes_key, limits, read_body};
use crate::search::{MAX_SEARCH_RESPONSE_BYTES, SearchEntities, civil_date, parse_extract_response, request_body};

const ENDPOINT: &str = "https://api.scaledown.xyz/v1/chat/completions";

/// Today's UTC date (`YYYY-MM-DD`); a clock before 1970 yields the epoch date, never a panic.
pub fn today_utc() -> String {
    civil_date(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()))
}

// The key never derives Serialize, and Debug is redacted; it never enters settings, receipts,
// URLs, request bodies or error strings. It is sent only in the `x-api-key` header.
pub struct ScaleDown {
    key: String,
}

impl std::fmt::Debug for ScaleDown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ScaleDown(<redacted>)")
    }
}

impl ScaleDown {
    pub fn new(key: String) -> Result<Self, String> {
        if key.is_empty() || key.len() > 4096 || key.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err("missing/invalid ScaleDown key".into());
        }
        Ok(Self { key })
    }

    /// Turn a natural-language library search into entities, using today's date for relative phrases.
    pub fn extract_search(&self, text: &str, cancel: &AtomicBool) -> Result<SearchEntities, String> {
        self.extract_search_on(text, Some(&today_utc()), cancel)
    }

    /// As [`ScaleDown::extract_search`] with an explicit `today` (`None` sends no date).
    pub fn extract_search_on(&self, text: &str, today: Option<&str>, cancel: &AtomicBool) -> Result<SearchEntities, String> {
        let body = serde_json::to_vec(&request_body(text, today)?).map_err(|_| "could not encode ScaleDown request")?;
        let limits = limits(cancel, 20);
        let url = Url::parse(ENDPOINT).map_err(|_| "invalid ScaleDown endpoint")?;
        let response = post_json(&url, &[("x-api-key", self.key.clone())], &body, &limits)
            .map_err(|_| "ScaleDown network failure; billing status unknown; no retry performed")?;
        match response.status {
            402 => return Err("ScaleDown credits exhausted (HTTP 402); top up the ScaleDown balance".into()),
            401 | 403 => return Err(format!("ScaleDown rejected the API key (HTTP {})", response.status)),
            _ => {}
        }
        let bytes = read_body(response, &limits, MAX_SEARCH_RESPONSE_BYTES)?;
        if echoes_key(&bytes, &self.key) {
            return Err("ScaleDown echoed credential; response discarded".into());
        }
        if cancel.load(Ordering::Relaxed) {
            return Err("search cancelled; no result published".into());
        }
        parse_extract_response(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_validation_and_redacted_debug() {
        assert!(ScaleDown::new(String::new()).is_err());
        assert!(ScaleDown::new("a b".into()).is_err());
        assert!(ScaleDown::new("a\nb".into()).is_err());
        assert!(ScaleDown::new("k".repeat(4097)).is_err());
        let client = ScaleDown::new("sd-secret-key".into()).unwrap();
        assert!(!format!("{client:?}").contains("secret"));
    }
}
