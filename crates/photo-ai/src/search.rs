//! Natural-language library search: pure request/response contract for a text-only extraction
//! model (ScaleDown `extract-1`). Only the typed search text (and today's date) is ever sent;
//! no photo, metadata or catalog data. The result maps onto fields of `lightcraft_catalog::Filter`
//! (`crates/catalog/src/query.rs`), emitted as a `library.filter` patch. Wasm-safe: no network,
//! no clock.
use serde::Serialize;
use serde_json::{Map, Value, json};

pub const SEARCH_MODEL: &str = "extract-1";
pub const MAX_SEARCH_CHARS: usize = 2000;
pub const MAX_SEARCH_RESPONSE_BYTES: usize = 64 * 1024;
/// Entities below this confidence are dropped (a missing confidence counts as certain).
pub const MIN_CONFIDENCE: f64 = 0.5;
const MAX_VALUE_CHARS: usize = 80;
const MAX_ENTITIES: usize = 64;
const MAX_TEXT_TOKENS: usize = 6;
const MAX_CONTENT_BYTES: usize = 48 * 1024;

/// Entity types the schema asks for. Each maps to a real `Filter` field:
/// keyword -> `keyword` (extra ones -> `text`), place -> `text` (matches location),
/// person -> `person`, camera -> `camera`, lens -> `lens`, date_from/date_to -> `dateFrom`/`dateTo`,
/// rating (+ rating_comparison) -> `rating`/`ratingOp`, flag -> `flag`, color_label -> `label`,
/// file_type -> `kind`, edited -> `edited`.
pub const ENTITY_TYPES: [(&str, &str); 13] = [
    (
        "keyword",
        "A subject or tag the photos are about, such as sunset, beach, wedding. One short word or phrase per value. Not a place, person or camera.",
    ),
    ("place", "A place name where the photos were taken: city, region, country, venue."),
    ("person", "The name of a person who appears in the photos."),
    ("camera", "A camera make or model, such as X-T5 or EOS R6."),
    ("lens", "A lens name or focal length description, such as 35mm f/1.4."),
    (
        "date_from",
        "Start of the capture date range as YYYY, YYYY-MM or YYYY-MM-DD. Resolve relative phrases (last summer, in April) using the Today line.",
    ),
    (
        "date_to",
        "End of the capture date range as YYYY, YYYY-MM or YYYY-MM-DD (inclusive). For a single year or month give the same value as date_from.",
    ),
    ("rating", "A star rating from 1 to 5, as a single digit."),
    ("rating_comparison", "How the rating applies: at least (4+, or better), exactly, or at most (or below)."),
    ("flag", "pick when the user wants picked or flagged photos, reject for rejected photos, none for unflagged."),
    ("color_label", "A colour label: red, yellow, green, blue or purple."),
    ("file_type", "raw, image or video."),
    ("edited", "yes when only edited photos are wanted, no when only unedited photos are wanted."),
];

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entity {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
    pub confidence: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchEntities {
    pub entities: Vec<Entity>,
    pub input_tokens: Option<u64>,
}

/// Strip control characters, trim, and cap to `max` chars (char-boundary safe).
fn clean(text: &str, max: usize) -> String {
    let spaced: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    spaced.trim().chars().take(max).collect::<String>().trim().to_owned()
}

/// A `YYYY`, `YYYY-MM` or `YYYY-MM-DD` date, validated; anything else is None.
pub fn valid_date(text: &str) -> Option<String> {
    let t = text.trim();
    let mut parts = t.split('-');
    let year = parts.next()?;
    let month = parts.next();
    let day = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let num = |s: &str, len: usize| -> Option<u32> { if s.len() == len && s.bytes().all(|b| b.is_ascii_digit()) { s.parse().ok() } else { None } };
    let y = num(year, 4).filter(|y| (1800..=2200).contains(y))?;
    let m = match month {
        Some(m) => Some(num(m, 2).filter(|m| (1..=12).contains(m))?),
        None => None,
    };
    let d = match day {
        Some(d) => Some(num(d, 2).filter(|d| (1..=31).contains(d))?),
        None => None,
    };
    Some(match (m, d) {
        (Some(m), Some(d)) => format!("{y:04}-{m:02}-{d:02}"),
        (Some(m), None) => format!("{y:04}-{m:02}"),
        _ => format!("{y:04}"),
    })
}

/// Civil UTC date (`YYYY-MM-DD`) for a Unix time in seconds (Hinnant's algorithm; no clock access).
pub fn civil_date(unix_secs: u64) -> String {
    let z = (unix_secs / 86_400).saturating_add(719_468);
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The ScaleDown Extract request body. Contains the (capped) search text and optional date context
/// only; the API key is a header and never enters the body.
pub fn request_body(text: &str, today: Option<&str>) -> Result<Value, String> {
    let query = clean(text, MAX_SEARCH_CHARS);
    if query.is_empty() {
        return Err("search text is empty".into());
    }
    let content = match today.and_then(valid_date) {
        Some(d) if d.len() == 10 => format!("Today is {d}.\nSearch: {query}"),
        _ => format!("Search: {query}"),
    };
    let mut properties = Map::new();
    for (name, description) in ENTITY_TYPES {
        properties.insert(name.into(), json!({"type": "string", "description": description}));
    }
    Ok(json!({
        "model": SEARCH_MODEL,
        "messages": [{"role": "user", "content": content}],
        "response_format": {"type": "json_schema", "json_schema": {
            "name": "ember_library_search",
            "schema": {"type": "object", "properties": properties}
        }}
    }))
}

fn push_entity(out: &mut Vec<Entity>, kind: &str, text: &str, confidence: f64) {
    let kind = kind.trim().to_ascii_lowercase();
    if out.len() >= MAX_ENTITIES || !ENTITY_TYPES.iter().any(|(name, _)| *name == kind) {
        return;
    }
    let text = clean(text, MAX_VALUE_CHARS);
    if text.is_empty() || !confidence.is_finite() || confidence < MIN_CONFIDENCE {
        return;
    }
    if out.iter().any(|e| e.kind == kind && e.text.eq_ignore_ascii_case(&text)) {
        return;
    }
    out.push(Entity { kind, text, confidence: confidence.min(1.0) });
}

/// Parse an Extract response (`choices[0].message.content` is itself a JSON string). Tolerant of
/// missing fields; never panics on malformed or oversized input.
pub fn parse_extract_response(bytes: &[u8]) -> Result<SearchEntities, String> {
    if bytes.len() > MAX_SEARCH_RESPONSE_BYTES {
        return Err("ScaleDown response exceeds byte limit".into());
    }
    let outer: Value = serde_json::from_slice(bytes).map_err(|_| "ScaleDown response is not JSON")?;
    if outer.get("error").is_some_and(|v| !v.is_null()) {
        return Err("ScaleDown returned an error".into());
    }
    let content = outer
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .and_then(|c| c.pointer("/message/content"))
        .and_then(Value::as_str)
        .filter(|s| s.len() <= MAX_CONTENT_BYTES)
        .ok_or("ScaleDown response has no usable content")?;
    let inner: Value = serde_json::from_str(content).map_err(|_| "ScaleDown content is not JSON")?;
    let mut out = Vec::new();
    if let Some(items) = inner.get("entities").and_then(Value::as_array) {
        for item in items.iter().take(MAX_ENTITIES * 2) {
            let kind = item.get("type").and_then(Value::as_str);
            let text = item.get("text").and_then(Value::as_str);
            let confidence = item.get("confidence").map_or(1.0, |c| c.as_f64().unwrap_or(f64::NAN));
            if let (Some(kind), Some(text)) = (kind, text) {
                push_entity(&mut out, kind, text, confidence);
            }
        }
    }
    if let Some(structured) = inner.get("structured_result").and_then(Value::as_object) {
        for (kind, value) in structured {
            match value {
                Value::String(s) => push_entity(&mut out, kind, s, 1.0),
                Value::Array(items) => {
                    for s in items.iter().take(30).filter_map(Value::as_str) {
                        push_entity(&mut out, kind, s, 1.0);
                    }
                }
                _ => {}
            }
        }
    }
    Ok(SearchEntities { entities: out, input_tokens: inner.get("input_tokens").and_then(Value::as_u64) })
}

fn values<'a>(found: &'a SearchEntities, kind: &'a str) -> impl Iterator<Item = &'a str> {
    found.entities.iter().filter(move |e| e.kind == kind).map(|e| e.text.as_str())
}

/// Free-text tokens: no `:` (it would turn a token into a fielded query such as `iso:>800`).
fn text_tokens(s: &str) -> impl Iterator<Item = String> + '_ {
    s.split(|c: char| c.is_whitespace() || c == ':').filter(|t| !t.is_empty()).map(str::to_owned)
}

fn lower(s: &str) -> String {
    s.trim().to_ascii_lowercase()
}

/// A `library.filter` patch for the entities. It sets every field this module manages (unset ones
/// to their no-constraint value) so the search replaces the previous search instead of stacking on
/// it; other filter fields (album, folder, rule set...) are untouched. Values that cannot match a
/// real `Filter` value are dropped, never passed through.
pub fn entities_to_filter_json(found: &SearchEntities) -> Value {
    let mut keywords = values(found, "keyword");
    let keyword = keywords.next().map(str::to_owned);
    let mut tokens: Vec<String> = values(found, "place").flat_map(text_tokens).collect();
    tokens.extend(keywords.flat_map(text_tokens));
    tokens.truncate(MAX_TEXT_TOKENS);

    let rating = values(found, "rating").find_map(|s| s.chars().find_map(|c| c.to_digit(10)).filter(|d| (1..=5).contains(d))).unwrap_or(0);
    let rating_op = match values(found, "rating_comparison").map(lower).next() {
        Some(c) if ["most", "below", "under", "<", "max", "up to"].iter().any(|k| c.contains(k)) => "atMost",
        Some(c) if ["exact", "only", "="].iter().any(|k| c.contains(k)) => "exactly",
        _ => "atLeast",
    };
    let flag = values(found, "flag").map(lower).find_map(|f| match f.as_str() {
        "none" | "unflagged" | "no flag" => Some("none"),
        s if s.contains("reject") => Some("reject"),
        s if s.contains("pick") || s.contains("flagged") => Some("pick"),
        _ => None,
    });
    let label =
        values(found, "color_label").map(lower).find_map(|l| ["red", "yellow", "green", "blue", "purple"].into_iter().find(|name| l.contains(name)));
    let kind = values(found, "file_type").map(lower).find_map(|k| match k.as_str() {
        s if s.contains("raw") => Some("raw"),
        s if s.contains("video") || s.contains("movie") => Some("video"),
        "image" | "jpeg" | "jpg" | "png" | "photo" | "heic" => Some("image"),
        _ => None,
    });
    let edited = values(found, "edited").map(lower).find_map(|e| match e.as_str() {
        "yes" | "true" | "edited" => Some(true),
        "no" | "false" | "unedited" | "not edited" => Some(false),
        _ => None,
    });
    let date_from = values(found, "date_from").find_map(valid_date);
    let date_to = values(found, "date_to").find_map(valid_date);

    json!({
        "text": tokens.join(" "),
        "rating": rating,
        "ratingOp": rating_op,
        "flag": flag,
        "label": label,
        "kind": kind,
        "edited": edited,
        "keyword": keyword,
        "person": values(found, "person").next(),
        "camera": values(found, "camera").next(),
        "lens": values(found, "lens").next(),
        "dateFrom": date_from,
        "dateTo": date_to,
        "date": null
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(inner: &Value) -> Vec<u8> {
        serde_json::to_vec(&json!({"choices":[{"message":{"role":"assistant","content":inner.to_string()}}]})).unwrap()
    }
    fn ent(kind: &str, text: &str, confidence: f64) -> Value {
        json!({"text": text, "type": kind, "confidence": confidence, "start": 0, "end": 1, "context": text})
    }
    fn filter_for(entities: &[(&str, &str)]) -> Value {
        let items: Vec<Value> = entities.iter().map(|(k, t)| ent(k, t, 0.9)).collect();
        let inner = json!({"entities": items, "structured_result": null, "input_tokens": 12, "ocr_text": null});
        entities_to_filter_json(&parse_extract_response(&response(&inner)).unwrap())
    }

    #[test]
    fn request_has_pinned_model_schema_and_only_the_search_text() {
        let body = request_body("sunset photos from Lisbon\u{7}", Some("2026-10-16")).unwrap();
        assert_eq!(body["model"], "extract-1");
        assert_eq!(body["messages"][0]["content"], "Today is 2026-10-16.\nSearch: sunset photos from Lisbon");
        let props = body["response_format"]["json_schema"]["schema"]["properties"].as_object().unwrap();
        assert_eq!(props.len(), ENTITY_TYPES.len());
        assert!(props.values().all(|p| p["type"] == "string"));
        let text = body.to_string();
        assert!(!text.contains("x-api-key") && !text.contains("$ref") && !text.contains("stream"));
        assert!(request_body("   \u{1}  ", None).is_err());
        let long = "\u{e9}".repeat(MAX_SEARCH_CHARS + 500);
        let capped = request_body(&long, Some("not a date")).unwrap();
        let content = capped["messages"][0]["content"].as_str().unwrap();
        assert_eq!(content.chars().count(), "Search: ".len() + MAX_SEARCH_CHARS);
    }

    #[test]
    fn documented_response_with_content_as_json_string_parses() {
        let inner = json!({"entities":[ent("place", "Lisbon", 0.97), ent("keyword", "sunset", 0.2)], "structured_result": null, "input_tokens": 31, "ocr_text": null});
        let parsed = parse_extract_response(&response(&inner)).unwrap();
        assert_eq!(parsed.input_tokens, Some(31));
        assert_eq!(parsed.entities.len(), 1, "low-confidence entity is dropped");
        assert_eq!(parsed.entities[0].kind, "place");
    }

    #[test]
    fn structured_result_and_missing_fields_are_tolerated() {
        let inner = json!({"structured_result": {"keyword": "sunset", "rating": "4", "bogus": "x", "flag": 5}});
        assert_eq!(parse_extract_response(&response(&inner)).unwrap().entities.len(), 2);
        assert!(parse_extract_response(&response(&json!({}))).unwrap().entities.is_empty());
        let sparse = json!({"entities":[{"type":"camera","text":"X-T5"}, {"type":"lens"}, {"text":"x"}]});
        assert_eq!(parse_extract_response(&response(&sparse)).unwrap().entities.len(), 1);
    }

    #[test]
    fn malformed_or_oversized_input_is_an_error_not_a_panic() {
        assert!(parse_extract_response(b"").is_err());
        assert!(parse_extract_response(b"not json").is_err());
        assert!(parse_extract_response(br#"{"choices":[]}"#).is_err());
        assert!(parse_extract_response(br#"{"choices":[{"message":{"content":"{oops"}}]}"#).is_err());
        assert!(parse_extract_response(br#"{"error":{"message":"x"}}"#).is_err());
        assert!(parse_extract_response(&vec![b' '; MAX_SEARCH_RESPONSE_BYTES + 1]).is_err());
        let huge = json!({"choices":[{"message":{"content":"x".repeat(MAX_CONTENT_BYTES + 1)}}]});
        assert!(parse_extract_response(&serde_json::to_vec(&huge).unwrap()).is_err());
    }

    #[test]
    fn sunset_lisbon_four_stars_picks() {
        let f = filter_for(&[
            ("keyword", "sunset"),
            ("place", "Lisbon"),
            ("date_from", "2025-06"),
            ("date_to", "2025-08"),
            ("rating", "4"),
            ("rating_comparison", "at least"),
            ("flag", "picked"),
        ]);
        assert_eq!(f["keyword"], "sunset");
        assert_eq!(f["text"], "Lisbon");
        assert_eq!(f["dateFrom"], "2025-06");
        assert_eq!(f["dateTo"], "2025-08");
        assert_eq!(f["rating"], 4);
        assert_eq!(f["ratingOp"], "atLeast");
        assert_eq!(f["flag"], "pick");
        assert!(f["label"].is_null() && f["kind"].is_null());
    }

    #[test]
    fn camera_lens_raw_and_label() {
        let f = filter_for(&[("camera", "X-T5"), ("lens", "35mm"), ("file_type", "RAW files"), ("color_label", "Red label"), ("edited", "no")]);
        assert_eq!(f["camera"], "X-T5");
        assert_eq!(f["lens"], "35mm");
        assert_eq!(f["kind"], "raw");
        assert_eq!(f["label"], "red");
        assert_eq!(f["edited"], false);
    }

    #[test]
    fn rating_at_most_and_rejects_and_person() {
        let f = filter_for(&[("rating", "2 stars"), ("rating_comparison", "or below"), ("flag", "rejected"), ("person", "Maria")]);
        assert_eq!(f["rating"], 2);
        assert_eq!(f["ratingOp"], "atMost");
        assert_eq!(f["flag"], "reject");
        assert_eq!(f["person"], "Maria");
    }

    #[test]
    fn invalid_values_are_dropped_and_text_cannot_become_a_fielded_token() {
        let f = filter_for(&[
            ("rating", "9"),
            ("flag", "maybe"),
            ("color_label", "teal"),
            ("file_type", "pdf"),
            ("date_from", "last summer"),
            ("date_to", "2025-13"),
            ("place", "iso:>800 New York"),
            ("keyword", "beach"),
            ("keyword", "dog"),
        ]);
        assert_eq!(f["rating"], 0);
        assert!(f["flag"].is_null() && f["label"].is_null() && f["kind"].is_null());
        assert!(f["dateFrom"].is_null() && f["dateTo"].is_null());
        assert_eq!(f["text"], "iso >800 New York dog");
        assert_eq!(f["keyword"], "beach");
    }

    #[test]
    fn dates_and_civil_conversion() {
        assert_eq!(valid_date("2026-04-12").as_deref(), Some("2026-04-12"));
        assert_eq!(valid_date("2026-4"), None);
        assert_eq!(valid_date("2026-04-12-01"), None);
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(951_782_400), "2000-02-29");
        assert!(civil_date(u64::MAX).len() > 4);
    }
}
