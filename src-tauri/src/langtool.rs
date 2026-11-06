//! Client minimal de l'API HTTP LanguageTool (`/v2/check`, `/v2/languages`).
//! Documentation : https://languagetool.org/http-api/

use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const MAX_SUGGESTIONS: usize = 8;

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("Coquille/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(25))
        .connect_timeout(Duration::from_secs(8))
        .build()
        .expect("client HTTP")
}

// ---- Réponse brute de LanguageTool (sous-ensemble) ----

#[derive(Debug, Deserialize)]
struct LtResponse {
    #[serde(default)]
    matches: Vec<LtMatch>,
    language: Option<LtLanguage>,
}

#[derive(Debug, Deserialize)]
struct LtLanguage {
    #[serde(default)]
    name: String,
    #[serde(default)]
    code: String,
    #[serde(rename = "detectedLanguage")]
    detected: Option<LtDetected>,
}

#[derive(Debug, Deserialize)]
struct LtDetected {
    #[serde(default)]
    name: String,
    #[serde(default)]
    code: String,
}

#[derive(Debug, Deserialize)]
struct LtMatch {
    #[serde(default)]
    message: String,
    #[serde(rename = "shortMessage", default)]
    short_message: String,
    offset: usize,
    length: usize,
    #[serde(default)]
    replacements: Vec<LtReplacement>,
    rule: LtRule,
}

#[derive(Debug, Deserialize)]
struct LtReplacement {
    value: String,
}

#[derive(Debug, Deserialize)]
struct LtRule {
    #[serde(default)]
    id: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "issueType", default)]
    issue_type: String,
    category: Option<LtCategory>,
}

#[derive(Debug, Deserialize)]
struct LtCategory {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
}

// ---- Ce que l'on renvoie au frontend ----

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct Issue {
    /// Décalage en unités UTF-16 (identique aux index des chaînes JavaScript).
    pub offset: usize,
    pub length: usize,
    /// Texte fautif tel qu'il apparaît dans le texte vérifié.
    pub original: String,
    pub message: String,
    pub short_message: String,
    pub replacements: Vec<String>,
    pub rule_id: String,
    pub rule_description: String,
    pub category: String,
    /// "orthographe" | "grammaire" | "typographie" | "style"
    pub kind: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct CheckResult {
    pub issues: Vec<Issue>,
    pub language: String,
    pub language_code: String,
}

fn classify(issue_type: &str, category_id: &str) -> &'static str {
    match (issue_type, category_id) {
        ("misspelling", _) | (_, "TYPOS") => "orthographe",
        (_, "TYPOGRAPHY") | (_, "PUNCTUATION") | ("typographical", _) | ("whitespace", _) => {
            "typographie"
        }
        (_, "STYLE") | (_, "REDUNDANCY") | (_, "REPETITIONS_STYLE") | (_, "PLAIN_ENGLISH")
        | ("style", _) | ("register", _) | ("locale-violation", _) => "style",
        _ => "grammaire",
    }
}

/// Extrait `text[offset..offset+len]` en unités UTF-16 (comme LanguageTool / JavaScript).
pub fn utf16_slice(units: &[u16], offset: usize, len: usize) -> Option<String> {
    let end = offset.checked_add(len)?;
    units.get(offset..end).map(String::from_utf16_lossy)
}

fn convert(text: &str, resp: LtResponse, cfg: &Config) -> CheckResult {
    let units: Vec<u16> = text.encode_utf16().collect();
    let issues = resp
        .matches
        .into_iter()
        .filter_map(|m| {
            let original = utf16_slice(&units, m.offset, m.length)?;
            let (cat_id, cat_name) = m
                .rule
                .category
                .map(|c| (c.id, c.name))
                .unwrap_or_default();
            let kind = classify(&m.rule.issue_type, &cat_id).to_string();
            if kind == "orthographe" && cfg.has_word(&original) {
                return None;
            }
            let mut replacements: Vec<String> = Vec::new();
            for r in m.replacements {
                if r.value != original && !replacements.contains(&r.value) {
                    replacements.push(r.value);
                }
                if replacements.len() >= MAX_SUGGESTIONS {
                    break;
                }
            }
            Some(Issue {
                offset: m.offset,
                length: m.length,
                original,
                message: m.message,
                short_message: m.short_message,
                replacements,
                rule_id: m.rule.id,
                rule_description: m.rule.description,
                category: cat_name,
                kind,
            })
        })
        .collect();

    let (language, language_code) = match resp.language {
        Some(l) => match l.detected {
            Some(d) if !d.code.is_empty() && d.code != l.code => (d.name, d.code),
            _ => (l.name, l.code),
        },
        None => (String::new(), String::new()),
    };
    CheckResult { issues, language, language_code }
}

pub async fn check(
    client: &reqwest::Client,
    cfg: &Config,
    text: &str,
    language: &str,
) -> Result<CheckResult, String> {
    if text.trim().is_empty() {
        return Ok(CheckResult { issues: vec![], language: String::new(), language_code: String::new() });
    }
    let lang = if language.trim().is_empty() { cfg.language.as_str() } else { language.trim() };
    let mut form: Vec<(&str, String)> = vec![
        ("text", text.to_string()),
        ("language", lang.to_string()),
        ("level", if cfg.picky { "picky" } else { "default" }.to_string()),
    ];
    if lang == "auto" {
        form.push(("preferredVariants", "fr,en-US,de-DE,pt-PT,ca-ES".into()));
    }
    if !cfg.disabled_rules.is_empty() {
        form.push(("disabledRules", cfg.disabled_rules.join(",")));
    }
    if !cfg.username.is_empty() && !cfg.api_key.is_empty() {
        form.push(("username", cfg.username.clone()));
        form.push(("apiKey", cfg.api_key.clone()));
    }

    let url = format!("{}/v2/check", cfg.server_url);
    let resp = client
        .post(&url)
        .form(&form)
        .send()
        .await
        .map_err(|e| describe_reqwest_error(&e, &cfg.server_url))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let body = body.trim();
        let hint = match status.as_u16() {
            413 => " (texte trop long pour ce serveur)",
            429 => " (trop de requêtes : le serveur public est limité, auto-héberge LanguageTool pour lever la limite)",
            _ => "",
        };
        return Err(format!(
            "Le serveur LanguageTool a répondu {status}{hint}{}",
            if body.is_empty() { String::new() } else { format!(" : {}", truncate(body, 300)) }
        ));
    }
    let parsed: LtResponse = resp
        .json()
        .await
        .map_err(|e| format!("Réponse inattendue du serveur LanguageTool : {e}"))?;
    Ok(convert(text, parsed, cfg))
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LtLang {
    pub name: String,
    pub code: String,
    #[serde(rename = "longCode")]
    pub long_code: String,
}

pub async fn languages(client: &reqwest::Client, server_url: &str) -> Result<Vec<LtLang>, String> {
    let url = format!("{server_url}/v2/languages");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| describe_reqwest_error(&e, server_url))?;
    if !resp.status().is_success() {
        return Err(format!("{url} a répondu {}", resp.status()));
    }
    resp.json::<Vec<LtLang>>()
        .await
        .map_err(|_| format!("{url} ne ressemble pas à un serveur LanguageTool"))
}

fn describe_reqwest_error(e: &reqwest::Error, server: &str) -> String {
    if e.is_timeout() {
        format!("Délai dépassé en contactant {server}")
    } else if e.is_connect() {
        format!("Impossible de joindre {server} (serveur éteint, mauvaise adresse ou réseau/Tailscale indisponible)")
    } else {
        format!("Erreur réseau avec {server} : {e}")
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "software": {"name": "LanguageTool", "version": "6.6"},
      "language": {"name": "French", "code": "fr", "detectedLanguage": {"name": "French", "code": "fr", "confidence": 0.99}},
      "matches": [
        {"message": "Faute de frappe possible trouvée.", "shortMessage": "Faute de frappe",
         "replacements": [{"value": "Bonjour"}, {"value": "Bonjours"}],
         "offset": 0, "length": 7,
         "rule": {"id": "FR_SPELLING_RULE", "description": "Faute de frappe possible", "issueType": "misspelling",
                  "category": {"id": "TYPOS", "name": "Faute de frappe possible"}}},
        {"message": "Accord du verbe.", "shortMessage": "",
         "replacements": [{"value": "sont"}],
         "offset": 20, "length": 3,
         "rule": {"id": "AGREEMENT", "description": "Accord", "issueType": "grammar",
                  "category": {"id": "GRAMMAR", "name": "Grammaire"}}},
        {"message": "Proxmox inconnu", "replacements": [],
         "offset": 28, "length": 7,
         "rule": {"id": "FR_SPELLING_RULE", "description": "x", "issueType": "misspelling",
                  "category": {"id": "TYPOS", "name": "x"}}}
      ]
    }"#;

    #[test]
    fn converts_and_filters_dictionary_words() {
        // 'é' et l'emoji vérifient le comptage en UTF-16.
        let text = "Bonjoru 🎉 les amis est là, Proxmox";
        let units: Vec<u16> = text.encode_utf16().collect();
        assert_eq!(utf16_slice(&units, 0, 7).unwrap(), "Bonjoru");
        // "est" commence à l'index UTF-16 20 (l'emoji compte pour 2 unités).
        assert_eq!(utf16_slice(&units, 20, 3).unwrap(), "est");
        assert_eq!(utf16_slice(&units, 28, 7).unwrap(), "Proxmox");

        let cfg = Config { dictionary: vec!["proxmox".into()], ..Default::default() };
        let resp: LtResponse = serde_json::from_str(SAMPLE).unwrap();
        let r = convert(text, resp, &cfg);
        assert_eq!(r.issues.len(), 2, "le mot du dictionnaire doit être filtré");
        assert_eq!(r.issues[0].original, "Bonjoru");
        assert_eq!(r.issues[0].kind, "orthographe");
        assert_eq!(r.issues[0].replacements, vec!["Bonjour", "Bonjours"]);
        assert_eq!(r.issues[1].original, "est");
        assert_eq!(r.issues[1].kind, "grammaire");
        assert_eq!(r.language_code, "fr");
    }

    #[test]
    fn out_of_range_matches_are_dropped() {
        let resp: LtResponse = serde_json::from_str(SAMPLE).unwrap();
        let r = convert("court", resp, &Config::default());
        assert!(r.issues.iter().all(|i| i.offset + i.length <= 5));
    }

    #[test]
    fn classification() {
        assert_eq!(classify("misspelling", "TYPOS"), "orthographe");
        assert_eq!(classify("typographical", "TYPOGRAPHY"), "typographie");
        assert_eq!(classify("style", "STYLE"), "style");
        assert_eq!(classify("grammar", "GRAMMAR"), "grammaire");
        assert_eq!(classify("uncategorized", "CASING"), "grammaire");
    }
}
