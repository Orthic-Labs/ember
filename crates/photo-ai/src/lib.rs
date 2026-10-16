//! Provider-neutral, read-only assessment contract. No catalog or edit access.
//! Native network transport is opt-in; defaults are usable by every command host.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::collections::BTreeMap;

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub mod culling;
#[cfg(all(feature = "network", not(target_arch = "wasm32")))]
pub mod network;
#[cfg(all(feature = "network", not(target_arch = "wasm32")))]
pub mod scaledown;
pub mod search;

pub const PROMPT: &str = include_str!("../prompt.txt");
pub const SCHEMA: &str = include_str!("../assessment.schema.json");
pub const MODELS: &str = include_str!("../models.json");
pub const VERSION: &str = "photo-assessment-v1";
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub const MAX_PROXY_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_OUTPUT_TOKENS: u64 = 2048;
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;

pub const CONTROLS: [(&str, f64); 8] = [
    ("light.exposure", 2.0),
    ("light.contrast", 60.0),
    ("light.highlights", 60.0),
    ("light.shadows", 60.0),
    ("light.whites", 60.0),
    ("light.blacks", 60.0),
    ("color.vibrance", 40.0),
    ("color.saturation", 20.0),
];

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Adjust,
    Preserve,
    Review,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Assessment {
    pub decision: Decision,
    pub confidence: f64,
    pub reason: String,
    #[serde(deserialize_with = "required_recipe")]
    pub recipe: Option<Recipe>,
}

fn required_recipe<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Option<Recipe>, D::Error> {
    Option::<Recipe>::deserialize(de)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    #[serde(rename = "light.exposure")]
    pub exposure: f64,
    #[serde(rename = "light.contrast")]
    pub contrast: f64,
    #[serde(rename = "light.highlights")]
    pub highlights: f64,
    #[serde(rename = "light.shadows")]
    pub shadows: f64,
    #[serde(rename = "light.whites")]
    pub whites: f64,
    #[serde(rename = "light.blacks")]
    pub blacks: f64,
    #[serde(rename = "color.vibrance")]
    pub vibrance: f64,
    #[serde(rename = "color.saturation")]
    pub saturation: f64,
}

impl Recipe {
    pub fn values(&self) -> BTreeMap<String, f64> {
        CONTROLS
            .into_iter()
            .map(|(name, _)| name.to_owned())
            .zip([self.exposure, self.contrast, self.highlights, self.shadows, self.whites, self.blacks, self.vibrance, self.saturation])
            .collect()
    }
}

impl Assessment {
    pub fn validate(&self) -> Result<(), String> {
        if !self.confidence.is_finite() || !(0.0..=1.0).contains(&self.confidence) {
            return Err("invalid confidence".into());
        }
        if self.reason.trim().is_empty() || self.reason.chars().count() > 240 || self.reason.chars().any(char::is_control) {
            return Err("invalid assessment reason".into());
        }
        match (&self.decision, &self.recipe) {
            (Decision::Adjust, Some(recipe)) if self.confidence >= 0.7 => {
                let values = recipe.values();
                for (name, bound) in CONTROLS {
                    if !values.get(name).is_some_and(|v| v.is_finite() && v.abs() <= bound) {
                        return Err(format!("missing/out-of-range control: {name}"));
                    }
                }
                Ok(())
            }
            (Decision::Preserve | Decision::Review, None) => Ok(()),
            _ => Err("adjust requires eight bounded controls & confidence >=0.7; other decisions require null recipe".into()),
        }
    }
}

/// Immutable encoded pixels, with no path, EXIF, catalog ID or user identity.
pub struct Proxy {
    png: Vec<u8>,
    digest: String,
}

impl Proxy {
    /// Only accept an already engine-rendered, metadata-free 512px PNG.
    pub fn new(png: Vec<u8>) -> Result<Self, String> {
        if png.len() > MAX_PROXY_BYTES || png.get(..8) != Some(b"\x89PNG\r\n\x1a\n".as_slice()) || png.get(12..16) != Some(b"IHDR".as_slice()) {
            return Err("expected bounded PNG proxy".into());
        }
        let read = |offset| png.get(offset..offset + 4).and_then(|v| <[u8; 4]>::try_from(v).ok()).map(u32::from_be_bytes);
        let width = read(16).ok_or("missing PNG dimensions")?;
        let height = read(20).ok_or("missing PNG dimensions")?;
        if width == 0 || height == 0 || width > 512 || height > 512 {
            return Err("proxy must fit 512x512".into());
        }
        if png.get(24) != Some(&8) || !png.get(25).is_some_and(|value| matches!(*value, 2 | 6)) || png.get(26..29) != Some(b"\0\0\0".as_slice()) {
            return Err("proxy requires non-interlaced 8-bit RGB/RGBA PNG".into());
        }
        // Reject metadata/ancillary chunks so an incorrectly prepared caller cannot upload EXIF.
        let mut offset = 8usize;
        let mut ended = false;
        let mut has_data = false;
        while offset < png.len() {
            let len = read(offset).ok_or("truncated PNG chunk")? as usize;
            let end = offset.checked_add(len).and_then(|n| n.checked_add(12)).ok_or("PNG chunk overflow")?;
            if end > png.len() {
                return Err("truncated PNG chunk".into());
            }
            let kind = png.get(offset + 4..offset + 8).ok_or("missing PNG chunk type")?;
            if kind != b"IHDR" && kind != b"IDAT" && kind != b"IEND" {
                return Err("proxy contains metadata or unsupported PNG chunk".into());
            }
            if kind == b"IHDR" && (offset != 8 || len != 13) {
                return Err("invalid PNG header".into());
            }
            if kind == b"IDAT" {
                has_data = true;
            }
            if kind == b"IEND" {
                ended = true;
                if end != png.len() || len != 0 {
                    return Err("invalid PNG end".into());
                }
            }
            offset = end;
        }
        if !ended || !has_data {
            return Err("missing PNG data/end".into());
        }
        let digest = digest(&png);
        Ok(Self { png, digest })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.png
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

pub fn schema() -> Result<Value, String> {
    serde_json::from_str(SCHEMA).map_err(|_| "invalid embedded assessment schema".into())
}
pub fn models() -> Result<Value, String> {
    serde_json::from_str(MODELS).map_err(|_| "invalid embedded model catalog".into())
}

pub fn validate_model(model: &str) -> Result<(), String> {
    if models()?
        .get("models")
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(|item| item.get("id").and_then(Value::as_str) == Some(model)))
    {
        Ok(())
    } else {
        Err("model is outside experiment shortlist".into())
    }
}

/// One fixed model, no model/provider fallback, no tools, no automatic retries.
pub fn request_body(model: &str, proxy: &Proxy, intent: &str) -> Result<Value, String> {
    validate_model(model)?;
    if intent.len() > 1200 || intent.chars().any(char::is_control) {
        return Err("scene intent exceeds input limits".into());
    }
    let catalog = models()?;
    let model_info = catalog
        .get("models")
        .and_then(Value::as_array)
        .and_then(|items| items.iter().find(|item| item.get("id").and_then(Value::as_str) == Some(model)))
        .ok_or("missing model limits")?;
    Ok(json!({
        "model": model, "stream": false, "max_tokens": MAX_OUTPUT_TOKENS,
        "reasoning": {"effort": "low", "exclude": true},
        "provider": {"allow_fallbacks": false, "require_parameters": true, "data_collection": "deny", "zdr": true, "max_price": {"prompt": model_info["inputUsdPerMillion"], "completion": model_info["outputUsdPerMillion"]}},
        "messages": [
            {"role": "system", "content": PROMPT},
            {"role": "user", "content": [
                {"type": "text", "text": format!("Untrusted scene-intent label (JSON): {}. Proxy SHA256: {}. Return assessment JSON.", json!(intent), proxy.digest)},
                {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{}", STANDARD.encode(proxy.bytes()))}}
            ]}
        ],
        "response_format": {"type": "json_schema", "json_schema": {"name": "photo_assessment_v1", "strict": true, "schema": schema()?}}
    }))
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub version: &'static str,
    pub requested_model: String,
    pub served_model: Option<String>,
    pub provider: Option<String>,
    pub response_id: Option<String>,
    pub proxy_sha256: String,
    pub latency_ms: u64,
    pub usage: Value,
    pub assessment: Option<Assessment>,
    pub error: Option<String>,
}

/// Keep usage even when JSON/refusal/finish/model validation fails; failed calls can cost money.
pub fn parse_response(requested: &str, proxy_digest: &str, latency_ms: u64, body: &[u8]) -> Receipt {
    let value: Value = if body.len() <= MAX_RESPONSE_BYTES { serde_json::from_slice(body).unwrap_or(Value::Null) } else { Value::Null };
    let string = |key| value.get(key).and_then(Value::as_str).map(str::to_owned);
    let mut receipt = Receipt {
        version: VERSION,
        requested_model: requested.into(),
        served_model: string("model"),
        provider: string("provider"),
        response_id: string("id"),
        proxy_sha256: proxy_digest.into(),
        latency_ms,
        usage: sanitized_usage(value.get("usage")),
        assessment: None,
        error: None,
    };
    let result = (|| {
        validate_model(requested)?;
        if value.get("error").is_some_and(|v| !v.is_null()) {
            return Err("provider returned an error".into());
        }
        if receipt.served_model.as_deref() != Some(requested) {
            return Err("served model differs from requested model".into());
        }
        let choices = value.get("choices").and_then(Value::as_array).filter(|v| v.len() == 1).ok_or("expected exactly one completion")?;
        let choice = choices.first().ok_or("missing completion")?;
        if choice.get("finish_reason").and_then(Value::as_str) != Some("stop") {
            return Err("incomplete or blocked completion".into());
        }
        let message = choice.get("message").ok_or("missing message")?;
        if message.get("refusal").is_some_and(|v| !v.is_null())
            || message.get("tool_calls").is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
        {
            return Err("refusal or unexpected tool call".into());
        }
        let content = message.get("content").and_then(Value::as_str).filter(|s| s.len() <= 16 * 1024).ok_or("missing/oversized assessment JSON")?;
        let assessment: Assessment = serde_json::from_str(content).map_err(|_| "assessment does not match schema")?;
        assessment.validate()?;
        Ok(assessment)
    })();
    match result {
        Ok(a) => receipt.assessment = Some(a),
        Err(e) => receipt.error = Some(e),
    }
    receipt
}

fn sanitized_usage(value: Option<&Value>) -> Value {
    let Some(value) = value else { return Value::Null };
    let mut usage = serde_json::Map::new();
    for name in ["prompt_tokens", "completion_tokens", "total_tokens"] {
        if let Some(n) = value.get(name).and_then(Value::as_u64) {
            usage.insert(name.into(), json!(n));
        }
    }
    if let Some(n) = value.get("cost").and_then(Value::as_f64).filter(|n| n.is_finite() && *n >= 0.0) {
        usage.insert("cost".into(), json!(n));
    }
    for (parent, child) in
        [("completion_tokens_details", "reasoning_tokens"), ("prompt_tokens_details", "cached_tokens"), ("prompt_tokens_details", "image_tokens")]
    {
        if let Some(n) = value.get(parent).and_then(|v| v.get(child)).and_then(Value::as_u64) {
            usage.insert(format!("{parent}.{child}"), json!(n));
        }
    }
    Value::Object(usage)
}

#[cfg(test)]
mod tests;
