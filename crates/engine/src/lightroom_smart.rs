//! Smart collections of a reference editor catalog, translated into Ember smart-album rules.
//!
//! The catalog stores a smart collection's criteria as a Lua table literal (`s = { { criteria = "rating", operation = ">=",
//! value = 3 }, combine = "intersect" }`), parsed here as data and never executed. A collection becomes a smart album only
//! when every criterion maps to an Ember rule with the same meaning; one unmapped criterion would silently change which
//! photos the album holds, so the collection then stays a regular album and each unmapped criterion is reported.
use lightcraft_catalog::rules::{Match, Rule, RuleSet};
use serde_json::{Value, json};

use crate::preset_import::{Lua, parse_lua};

/// Deepest nesting of rule groups followed, and most rules read from one collection.
const MAX_DEPTH: usize = 8;
const MAX_RULES: usize = 256;

/// The rules of a smart collection's stored criteria, or the reasons some criterion has no Ember rule.
pub(crate) fn translate(content: &str) -> Result<RuleSet, Vec<String>> {
    let root = parse_lua(content).map_err(|e| vec![format!("unreadable criteria: {e}")])?;
    let mut why = Vec::new();
    let mut count = 0;
    let set = group(&root, 0, &mut count, &mut why);
    match set {
        Some(set) if why.is_empty() && !set.rules.is_empty() => Ok(set),
        _ => {
            if why.is_empty() {
                why.push("no criteria".into());
            }
            Err(why)
        }
    }
}

fn group(table: &Lua, depth: usize, count: &mut usize, why: &mut Vec<String>) -> Option<RuleSet> {
    let Lua::Table(items, _) = table else {
        why.push("criteria are not a table".into());
        return None;
    };
    if depth > MAX_DEPTH {
        why.push("criteria nested too deeply".into());
        return None;
    }
    let mode = match table.get("combine").and_then(Lua::str) {
        Some("intersect") | None => Match::All,
        Some("union") => Match::Any,
        Some("exclude") => Match::None,
        Some(other) => {
            why.push(format!("combine mode `{other}`"));
            return None;
        }
    };
    let mut rules = Vec::new();
    for item in items {
        *count += 1;
        if *count > MAX_RULES {
            why.push("more than 256 criteria".into());
            return None;
        }
        if item.get("criteria").is_some() {
            if let Some(rule) = criterion(item, why) {
                rules.push(rule);
            }
        } else if let Some(inner) = group(item, depth + 1, count, why) {
            rules.push(Rule::Group { group: inner });
        }
    }
    Some(RuleSet { mode, rules })
}

fn num(v: Option<&Lua>) -> Option<f64> {
    match v {
        Some(Lua::Num(n)) if n.is_finite() => Some(*n),
        Some(Lua::Str(s)) => s.trim().parse::<f64>().ok().filter(|n| n.is_finite()),
        _ => None,
    }
}

fn string(v: Option<&Lua>) -> Option<String> {
    match v {
        Some(Lua::Str(s)) => Some(s.clone()),
        Some(Lua::Num(n)) if n.is_finite() => Some(format!("{n}")),
        _ => None,
    }
}

fn field(field: &str, op: &str, value: Value) -> Rule {
    Rule::Field { field: field.into(), op: op.into(), value }
}

fn criterion(item: &Lua, why: &mut Vec<String>) -> Option<Rule> {
    let name = item.get("criteria").and_then(Lua::str).unwrap_or("");
    let op = item.get("operation").and_then(Lua::str).unwrap_or("");
    let (v1, v2) = (item.get("value"), item.get("value2"));
    let mut unmapped = || {
        why.push(format!("criterion `{name}` {op}"));
        None
    };
    let compare = |op: &str| match op {
        "==" => Some("is"),
        "!=" => Some("isNot"),
        ">" => Some("gt"),
        ">=" => Some("gte"),
        "<" => Some("lt"),
        "<=" => Some("lte"),
        _ => None,
    };
    match name {
        "rating" | "iso" | "focalLength" => {
            let ember = name;
            if op == "in" {
                return match (num(v1), num(v2)) {
                    (Some(a), Some(b)) => Some(field(ember, "between", json!([a, b]))),
                    _ => unmapped(),
                };
            }
            match (compare(op), num(v1)) {
                (Some(op), Some(n)) => Some(field(ember, op, json!(n))),
                _ => unmapped(),
            }
        }
        "pick" => {
            let choice = match num(v1) {
                Some(n) if n == 1.0 => "pick",
                Some(n) if n == -1.0 => "reject",
                Some(n) if n == 0.0 => "none",
                _ => return unmapped(),
            };
            match op {
                "==" => Some(field("flag", "is", json!(choice))),
                "!=" => Some(field("flag", "isNot", json!(choice))),
                _ => unmapped(),
            }
        }
        "labelColor" => {
            let color = string(v1).map(|s| s.to_lowercase()).filter(|c| ["red", "yellow", "green", "blue", "purple", "none"].contains(&c.as_str()));
            match (op, color) {
                ("==", Some(c)) => Some(field("label", "is", json!(c))),
                ("!=", Some(c)) => Some(field("label", "isNot", json!(c))),
                _ => unmapped(),
            }
        }
        "fileFormat" => {
            let kind = match string(v1).as_deref() {
                Some("RAW") => "raw",
                Some("VIDEO") => "video",
                _ => return unmapped(),
            };
            match op {
                "==" => Some(field("kind", "is", json!(kind))),
                "!=" => Some(field("kind", "isNot", json!(kind))),
                _ => unmapped(),
            }
        }
        "captureTime" => {
            let date = |v: Option<&Lua>| string(v).filter(|s| !s.trim().is_empty());
            match op {
                "==" | ">" | "<" => {
                    let ember = match op {
                        "==" => "is",
                        ">" => "after",
                        _ => "before",
                    };
                    match date(v1) {
                        Some(d) => Some(field("captureDate", ember, json!(d))),
                        None => unmapped(),
                    }
                }
                "in" => match (date(v1), date(v2)) {
                    (Some(a), Some(b)) => Some(field("captureDate", "between", json!([a, b]))),
                    _ => unmapped(),
                },
                _ => unmapped(),
            }
        }
        "filename" | "copyname" | "title" | "caption" | "creator" | "copyright" | "camera" | "lens" | "city" | "state" | "country" | "keywords" => {
            let ember = match name {
                "filename" => "fileName",
                "copyname" => "copyName",
                other => other,
            };
            let Some(text) = string(v1).filter(|s| !s.trim().is_empty()) else { return unmapped() };
            // Word matching (`any`, `all`, `words`) splits several words; a phrase has no single Ember rule.
            let ember_op = match op {
                "any" | "all" | "words" if !text.contains(char::is_whitespace) => "contains",
                "noneOf" if !text.contains(char::is_whitespace) => "notContains",
                "==" => "is",
                "!=" if name != "keywords" => "isNot",
                "beginsWith" => "startsWith",
                "endsWith" if name != "keywords" => "endsWith",
                _ => return unmapped(),
            };
            Some(field(ember, ember_op, json!(text)))
        }
        _ => unmapped(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_supported_criterion_with_its_combine_mode() {
        let set = translate(
            "s = { { criteria = \"rating\", operation = \">=\", value = 3, value2 = 0 }, \
             { criteria = \"labelColor\", operation = \"==\", value = \"Red\" }, \
             { criteria = \"captureTime\", operation = \"in\", value = \"2024-01-01\", value2 = \"2024-02-01\" }, \
             { combine = \"union\", { criteria = \"keywords\", operation = \"words\", value = \"goa\" }, \
               { criteria = \"pick\", operation = \"==\", value = 1 } }, \
             combine = \"intersect\" }",
        )
        .unwrap();
        assert_eq!(set.mode, Match::All);
        assert_eq!(set.rules.len(), 4);
        assert_eq!(set.rules[0], field("rating", "gte", json!(3.0)));
        assert_eq!(set.rules[1], field("label", "is", json!("red")));
        assert_eq!(set.rules[2], field("captureDate", "between", json!(["2024-01-01", "2024-02-01"])));
        let Rule::Group { group } = &set.rules[3] else { panic!("expected a group") };
        assert_eq!(group.mode, Match::Any);
        assert_eq!(group.rules, vec![field("keywords", "contains", json!("goa")), field("flag", "is", json!("pick"))]);
        assert_eq!(translate("s = { { criteria = \"iso\", operation = \"<\", value = 800 }, combine = \"exclude\" }").unwrap().mode, Match::None);
    }

    #[test]
    fn one_unmapped_criterion_keeps_the_whole_collection_regular() {
        let why = translate(
            "s = { { criteria = \"rating\", operation = \">\", value = 1 }, { criteria = \"develop_preset\", operation = \"==\", value = \"x\" }, \
             { criteria = \"keywords\", operation = \"all\", value = \"two words\" }, combine = \"intersect\" }",
        )
        .unwrap_err();
        assert_eq!(why.len(), 2, "{why:?}");
        assert!(why[0].contains("develop_preset") && why[1].contains("keywords"), "{why:?}");
        assert!(translate("s = {}").is_err());
        assert!(translate("not a table").is_err());
        assert!(translate("s = { { criteria = \"rating\", operation = \"==\", value = 2 }, combine = \"other\" }").is_err());
    }

    #[test]
    fn hostile_nesting_and_size_are_bounded() {
        let deep =
            format!("s = {}{{ criteria = \"rating\", operation = \"==\", value = 1 }}{}", "{ combine = \"union\", ".repeat(12), "}".repeat(12));
        assert!(translate(&deep).is_err());
        let many = format!("s = {{ {} }}", "{ criteria = \"rating\", operation = \"==\", value = 1 },".repeat(300));
        assert!(translate(&many).is_err());
    }
}
