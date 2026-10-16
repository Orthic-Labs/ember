//! Natural-language search, local and offline: ranks the library's photos by how many words of a
//! typed phrase their own metadata matches (keywords, people, title, caption, places, descriptions).
//! Stop words are dropped, plurals and `-ing` / `-ed` endings are stemmed, and a small synonym table
//! joins words that mean the same thing. Pure: no network, no clock, no file access.
use crate::{Catalog, Photo, PhotoId};

/// Words that carry no meaning for a search (stems are compared too, so `photos` and `this` drop).
const STOP: &[&str] = &[
    "a", "an", "the", "of", "on", "in", "at", "with", "and", "or", "to", "from", "for", "by", "my", "our", "me", "i", "is", "are", "was", "were",
    "that", "this", "some", "all", "show", "find", "photo", "picture", "shot", "image", "pic", "when", "where",
];

/// Words that mean the same thing for search. A query word matches any photo word in its group.
const SYNONYMS: &[&[&str]] = &[
    &["dog", "puppy", "pup", "canine", "hound"],
    &["cat", "kitten", "kitty", "feline"],
    &["beach", "shore", "seaside", "coast", "coastline"],
    &["sunset", "dusk", "sundown", "twilight"],
    &["sea", "ocean", "wave", "surf"],
    &["mountain", "peak", "summit", "alp"],
    &["city", "urban", "town"],
    &["forest", "woods", "woodland", "tree"],
    &["flower", "blossom", "bloom"],
    &["child", "kid", "baby", "toddler"],
    &["night", "nighttime", "midnight"],
    &["snow", "winter"],
    &["car", "automobile", "vehicle"],
];

/// Bounds the work one query can cause.
const MAX_TERMS: usize = 12;

/// One searched word: the stems it matches, or a year that matches the capture date.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Term {
    accept: Vec<String>,
    year: Option<String>,
}

/// Lowercase stem of one word: plurals, `-ing` and `-ed` endings removed. Never panics.
pub fn stem(word: &str) -> String {
    let w = word.to_lowercase();
    let n = w.chars().count();
    let out = if n > 4 && w.ends_with("ies") {
        w.strip_suffix("ies").map(|base| format!("{base}y"))
    } else if n > 4 && ["sses", "xes", "ches", "shes"].iter().any(|s| w.ends_with(s)) {
        w.strip_suffix("es").map(str::to_owned)
    } else if n > 3 && w.ends_with('s') && !w.ends_with("ss") {
        w.strip_suffix('s').map(str::to_owned)
    } else if n > 5 && w.ends_with("ing") {
        w.strip_suffix("ing").map(str::to_owned)
    } else if n > 4 && w.ends_with("ed") {
        w.strip_suffix("ed").map(str::to_owned)
    } else {
        None
    };
    out.unwrap_or(w)
}

/// The stems a query word matches: its synonym group's stems, or its own stem.
fn accepted(word: &str) -> Vec<String> {
    let s = stem(word);
    match SYNONYMS.iter().find(|group| group.iter().any(|g| stem(g) == s)) {
        Some(group) => group.iter().map(|g| stem(g)).collect(),
        None => vec![s],
    }
}

/// The searched terms of `query`, in order. Empty when nothing searchable is typed.
pub fn terms(query: &str) -> Vec<Term> {
    let mut out = Vec::new();
    for word in query.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).take(MAX_TERMS) {
        let lower = word.to_lowercase();
        if lower.len() == 4 && lower.bytes().all(|b| b.is_ascii_digit()) {
            out.push(Term { accept: Vec::new(), year: Some(lower) });
            continue;
        }
        if STOP.contains(&lower.as_str()) || STOP.contains(&stem(&lower).as_str()) {
            continue;
        }
        out.push(Term { accept: accepted(&lower), year: None });
    }
    out
}

/// The words a photo can be found by, with how much a match counts (3 keywords, people and title;
/// 2 caption and places; 1 long descriptions). Each word is stemmed.
fn photo_words(p: &Photo) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut push = |weight: u32, text: &str| {
        for w in text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
            out.push((weight, stem(w)));
        }
    };
    for keyword in &p.meta.keywords {
        push(3, keyword);
    }
    for person in p.people() {
        push(3, person);
    }
    push(3, &p.meta.title);
    push(2, &p.meta.caption);
    for place in [&p.meta.location, &p.meta.city, &p.meta.state, &p.meta.country] {
        push(2, place);
    }
    push(1, &p.meta.alt_text);
    push(1, &p.meta.extended_description);
    out
}

/// Score of one photo: for each term, the best weight of a word that matches it, summed.
fn score_words(words: &[(u32, String)], captured: Option<&str>, terms: &[Term]) -> u32 {
    terms
        .iter()
        .map(|term| {
            let mut best = 0;
            for (weight, word) in words {
                if *weight > best && term.accept.iter().any(|a| a == word) {
                    best = *weight;
                }
            }
            if let Some(year) = &term.year
                && captured.is_some_and(|c| c.starts_with(year.as_str()))
            {
                best = best.max(2);
            }
            best
        })
        .sum()
}

/// Score of one photo for `query` (0 = no word matches).
pub fn score(p: &Photo, query: &str) -> u32 {
    score_words(&photo_words(p), p.captured.as_deref(), &terms(query))
}

/// The library's photos that match `query`, best first (ties by id). Empty for an empty query.
pub fn rank(cat: &Catalog, query: &str) -> Vec<(PhotoId, u32)> {
    let terms = terms(query);
    if terms.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<(PhotoId, u32)> = cat
        .photos()
        .filter(|p| p.in_library())
        .filter_map(|p| {
            let s = score_words(&photo_words(p), p.captured.as_deref(), &terms);
            (s > 0).then_some((p.id, s))
        })
        .collect();
    hits.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    hits
}
