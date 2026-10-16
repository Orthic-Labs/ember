//! Native-only OpenRouter adapter. Run on a worker/CLI thread, never the UI owner.
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use lightcraft_fetch::http::{Limits, Url, get, post_json};
use serde_json::Value;

use crate::{MAX_RESPONSE_BYTES, Proxy, Receipt, culling, parse_response, request_body};

const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

// Secrets never derive Debug/Serialize or enter settings, receipts, URLs or error strings.
pub struct OpenRouter {
    key: String,
}

impl OpenRouter {
    pub fn new(key: String) -> Result<Self, String> {
        if key.is_empty() || key.len() > 4096 || key.chars().any(char::is_control) {
            return Err("missing/invalid OpenRouter key".into());
        }
        Ok(Self { key })
    }

    /// Capability preflight uses public model metadata, not private images or credentials.
    pub fn catalog(&self, cancel: &AtomicBool) -> Result<Value, String> {
        let limits = limits(cancel, 20);
        let url = Url::parse("https://openrouter.ai/api/v1/models").map_err(|_| "invalid provider endpoint")?;
        let response = get(&url, &[], &limits).map_err(|_| "model catalog network failure")?;
        let bytes = read_body(response, &limits, 4 * 1024 * 1024)?;
        serde_json::from_slice(&bytes).map_err(|_| "invalid model catalog".into())
    }

    pub fn assess(&self, model: &str, proxy: &Proxy, intent: &str, cancel: &AtomicBool) -> Result<Receipt, String> {
        let body = serde_json::to_vec(&request_body(model, proxy, intent)?).map_err(|_| "could not encode provider request")?;
        let limits = limits(cancel, 45);
        let url = Url::parse(ENDPOINT).map_err(|_| "invalid provider endpoint")?;
        let start = Instant::now();
        let response = post_json(&url, &[("Authorization", format!("Bearer {}", self.key))], &body, &limits)
            .map_err(|_| "assessment network failure; billing status unknown; no retry performed")?;
        let bytes = read_body(response, &limits, MAX_RESPONSE_BYTES)?;
        if echoes_key(&bytes, &self.key) {
            return Err("provider echoed credential; response discarded".into());
        }
        if cancel.load(Ordering::Relaxed) {
            return Err("assessment cancelled; no result published".into());
        }
        Ok(parse_response(model, proxy.digest(), start.elapsed().as_millis().min(u64::MAX as u128) as u64, &bytes))
    }

    /// Compare one bounded burst through the same strict, read-only provider path.
    /// Budget admits a frozen-rate reserve before upload; this client cannot enforce
    /// provider spend through `max_price`, so receipts preserve reported/unknown billing.
    pub fn cull_burst(
        &self,
        model: &str,
        proxies: &[culling::ProxyInput<'_>],
        options: culling::RequestOptions,
        cancel: &AtomicBool,
    ) -> Result<culling::Receipt, String> {
        let body = serde_json::to_vec(&culling::request_body(model, proxies, options)?).map_err(|_| "could not encode culling provider request")?;
        let limits = limits(cancel, options.deadline_secs);
        let url = Url::parse(ENDPOINT).map_err(|_| "invalid provider endpoint")?;
        let start = Instant::now();
        let response = post_json(&url, &[("Authorization", format!("Bearer {}", self.key))], &body, &limits)
            .map_err(|_| "culling network failure; billing status unknown; no retry performed")?;
        let bytes =
            read_body(response, &limits, MAX_RESPONSE_BYTES).map_err(|error| format!("{error}; billing status unknown; no retry performed"))?;
        if echoes_key(&bytes, &self.key) {
            return Err("provider echoed credential; response discarded; billing status unknown; no retry performed".into());
        }
        if cancel.load(Ordering::Relaxed) {
            return Err("culling cancelled; no result published; billing status unknown; no retry performed".into());
        }
        let latency_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
        Ok(culling::parse_response(model, proxies, options, latency_ms, &bytes))
    }
}

pub(crate) fn echoes_key(bytes: &[u8], key: &str) -> bool {
    fn contains(value: &Value, key: &str) -> bool {
        match value {
            Value::String(text) => text.contains(key),
            Value::Array(items) => items.iter().any(|item| contains(item, key)),
            Value::Object(items) => items.iter().any(|(name, item)| name.contains(key) || contains(item, key)),
            _ => false,
        }
    }
    std::str::from_utf8(bytes).is_ok_and(|text| text.contains(key)) || serde_json::from_slice::<Value>(bytes).is_ok_and(|value| contains(&value, key))
}

pub(crate) fn limits(cancel: &AtomicBool, seconds: u64) -> Limits<'_> {
    Limits {
        connect: Duration::from_secs(5),
        stall: Duration::from_secs(30),
        cancel,
        deadline: Instant::now().checked_add(Duration::from_secs(seconds)),
    }
}

pub(crate) fn read_body(mut response: lightcraft_fetch::http::Response, limits: &Limits<'_>, cap: usize) -> Result<Vec<u8>, String> {
    if !(200..300).contains(&response.status) {
        return Err(format!("provider HTTP {}; no retry performed", response.status));
    }
    if response.content_length().is_some_and(|n| n > cap as u64) {
        return Err("provider response exceeds byte limit".into());
    }
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let n = response.read(&mut buffer, limits).map_err(|_| "provider response interrupted; billing status unknown; no retry performed")?;
        if n == 0 {
            break;
        }
        if bytes.len().saturating_add(n) > cap {
            return Err("provider response exceeds byte limit".into());
        }
        bytes.extend_from_slice(buffer.get(..n).ok_or("invalid response read")?);
    }
    Ok(bytes)
}

/// Model-level support is a preflight; require_parameters enforces endpoint-level support.
pub fn preflight(catalog: &Value, model: &str) -> Result<Value, String> {
    crate::validate_model(model)?;
    let item = catalog
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| items.iter().find(|item| item.get("id").and_then(Value::as_str) == Some(model)))
        .ok_or("model unavailable")?;
    let has = |value: Option<&Value>, target| value.and_then(Value::as_array).is_some_and(|v| v.iter().any(|x| x.as_str() == Some(target)));
    if !has(item.pointer("/architecture/input_modalities"), "image") || !has(item.get("supported_parameters"), "structured_outputs") {
        return Err("model lacks image/structured-output capability".into());
    }
    let expected = crate::models()?
        .get("models")
        .and_then(Value::as_array)
        .and_then(|v| v.iter().find(|x| x.get("id").and_then(Value::as_str) == Some(model)))
        .cloned()
        .ok_or("missing pinned metadata")?;
    if item.get("canonical_slug") != expected.get("canonicalSlug") {
        return Err("model revision changed; refresh experiment explicitly".into());
    }
    Ok(item.clone())
}

#[cfg(test)]
mod tests {
    #[test]
    fn escaped_credentials_cannot_enter_receipts() {
        assert!(super::echoes_key(br#"{"usage":{"extra":"sk-\u0074est"}}"#, "sk-test"));
        assert!(super::echoes_key(br#"{"sk-\u0074est":"value"}"#, "sk-test"));
        assert!(!super::echoes_key(br#"{"usage":{"cost":0.001}}"#, "sk-test"));
    }
}
