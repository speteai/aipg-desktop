//! Finds private values in text that is about to leave the computer (API
//! keys, passwords, e-mail addresses, IBANs, card numbers, phone numbers) and
//! swaps them for placeholders, so the machine that answers a grid request
//! never sees the real values. The placeholders are put back in the answer.
//!
//! The same rules exist in `src/main.js` for the chat window; keep the two in
//! step. This is a safety net for clearly shaped values, not a guarantee:
//! confidential prose is not recognised.

use std::sync::OnceLock;

use regex::Regex;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Secret,
    Iban,
    Card,
    Email,
    Phone,
}

impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Kind::Secret => "SECRET",
            Kind::Iban => "IBAN",
            Kind::Card => "CARD",
            Kind::Email => "EMAIL",
            Kind::Phone => "PHONE",
        }
    }
    fn label(self, n: usize) -> &'static str {
        match (self, n == 1) {
            (Kind::Secret, true) => "key or password",
            (Kind::Secret, false) => "keys or passwords",
            (Kind::Iban, true) => "IBAN",
            (Kind::Iban, false) => "IBANs",
            (Kind::Card, true) => "card number",
            (Kind::Card, false) => "card numbers",
            (Kind::Email, true) => "e-mail address",
            (Kind::Email, false) => "e-mail addresses",
            (Kind::Phone, true) => "phone number",
            (Kind::Phone, false) => "phone numbers",
        }
    }
}

struct Rule {
    kind: Kind,
    re: Regex,
    /// capture group that holds the private value (0 = whole match)
    group: usize,
}

fn rules() -> &'static Vec<Rule> {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let r = |kind, pat: &str, group| Rule { kind, re: Regex::new(pat).expect("privacy rule"), group };
        vec![
            r(Kind::Secret, r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----", 0),
            r(Kind::Secret, r"\bgrid_[A-Za-z0-9_-]{16,}", 0),
            r(Kind::Secret, r"\bsk-[A-Za-z0-9_-]{20,}", 0),
            r(Kind::Secret, r"\bgh[pousr]_[A-Za-z0-9]{30,}", 0),
            r(Kind::Secret, r"\bgithub_pat_[A-Za-z0-9_]{30,}", 0),
            r(Kind::Secret, r"\bAKIA[0-9A-Z]{16}\b", 0),
            r(Kind::Secret, r"\bxox[baprs]-[A-Za-z0-9-]{10,}", 0),
            r(Kind::Secret, r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}", 0),
            r(Kind::Secret, r"\b0x[0-9a-fA-F]{64}\b", 0),
            r(
                Kind::Secret,
                r#"(?i)\b(?:pass(?:word|wort)?|pwd|kennwort|secret|api[_-]?key|token)\b\s*[:=]\s*["']?([^\s"',;]{4,})"#,
                1,
            ),
            r(Kind::Iban, r"\b[A-Z]{2}\d{2}(?: ?[A-Z0-9]{4}){2,7}(?: ?[A-Z0-9]{1,4})?\b", 0),
            r(Kind::Card, r"\b\d(?:[ -]?\d){12,18}\b", 0),
            r(Kind::Email, r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b", 0),
            r(Kind::Phone, r"\+\d{1,3}[\d \-/()]{7,18}\d", 0),
        ]
    })
}

fn iban_ok(s: &str) -> bool {
    let c: Vec<char> = s.chars().filter(|c| !c.is_whitespace()).collect();
    if c.len() < 15 || c.len() > 34 {
        return false;
    }
    let mut rem: u32 = 0;
    for ch in c[4..].iter().chain(c[..4].iter()) {
        let v = match ch {
            '0'..='9' => *ch as u32 - '0' as u32,
            'A'..='Z' => *ch as u32 - 'A' as u32 + 10,
            _ => return false,
        };
        rem = if v >= 10 { (rem * 100 + v) % 97 } else { (rem * 10 + v) % 97 };
    }
    rem == 1
}

fn luhn_ok(s: &str) -> bool {
    let d: Vec<u32> = s.chars().filter_map(|c| c.to_digit(10)).collect();
    if d.len() < 13 || d.len() > 19 {
        return false;
    }
    let sum: u32 = d
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &x)| if i % 2 == 1 { let y = x * 2; if y > 9 { y - 9 } else { y } } else { x })
        .sum();
    sum % 10 == 0
}

/// Private values in `text` as (start, end, kind), in order, not overlapping.
/// Where rules overlap, the earlier rule in the list wins.
pub fn find(text: &str) -> Vec<(usize, usize, Kind)> {
    let mut hits: Vec<(usize, usize, Kind, usize)> = Vec::new();
    for (prio, rule) in rules().iter().enumerate() {
        for cap in rule.re.captures_iter(text) {
            let Some(m) = cap.get(rule.group) else { continue };
            let ok = match rule.kind {
                Kind::Iban => iban_ok(m.as_str()),
                Kind::Card => luhn_ok(m.as_str()),
                _ => true,
            };
            if ok {
                hits.push((m.start(), m.end(), rule.kind, prio));
            }
        }
    }
    hits.sort_by_key(|h| (h.3, h.0));
    let mut kept: Vec<(usize, usize, Kind)> = Vec::new();
    for (s, e, k, _) in hits {
        if !kept.iter().any(|(ks, ke, _)| s < *ke && *ks < e) {
            kept.push((s, e, k));
        }
    }
    kept.sort_by_key(|h| h.0);
    kept
}

/// "2 e-mail addresses, 1 key or password"
pub fn summary(hits: &[(usize, usize, Kind)]) -> String {
    let mut parts = Vec::new();
    for kind in [Kind::Secret, Kind::Iban, Kind::Card, Kind::Email, Kind::Phone] {
        let n = hits.iter().filter(|h| h.2 == kind).count();
        if n > 0 {
            parts.push(format!("{n} {}", kind.label(n)));
        }
    }
    parts.join(", ")
}

/// Placeholders handed out for one request; the same value always gets the
/// same placeholder.
#[derive(Default)]
pub struct Mask {
    pairs: Vec<(String, String)>, // (placeholder, original)
}

impl Mask {
    /// Replace every private value in `text` by its placeholder.
    pub fn apply(&mut self, text: &str) -> String {
        let hits = find(text);
        if hits.is_empty() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut pos = 0;
        for (s, e, kind) in hits {
            out.push_str(&text[pos..s]);
            let value = &text[s..e];
            let ph = match self.pairs.iter().find(|(_, v)| v == value) {
                Some((ph, _)) => ph.clone(),
                None => {
                    let n = self.pairs.iter().filter(|(p, _)| p.starts_with(&format!("[[{}_", kind.tag()))).count() + 1;
                    let ph = format!("[[{}_{n}]]", kind.tag());
                    self.pairs.push((ph.clone(), value.to_string()));
                    ph
                }
            };
            out.push_str(&ph);
            pos = e;
        }
        out.push_str(&text[pos..]);
        out
    }

    /// Put the original values back into an answer. Models sometimes drop a
    /// bracket, so `[EMAIL_1]` is accepted as well.
    pub fn restore(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (ph, value) in &self.pairs {
            out = out.replace(ph, value);
            out = out.replace(&ph[1..ph.len() - 1], value);
        }
        out
    }
}

/// Sentence that tells the model how to treat the placeholders.
pub const PLACEHOLDER_NOTE: &str = "Tokens such as [[EMAIL_1]] or [[SECRET_1]] in the text stand for private values that were removed. Use them exactly as written wherever the value belongs; do not alter, explain or guess them.";
