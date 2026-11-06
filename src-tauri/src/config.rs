use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

pub const PUBLIC_SERVER: &str = "https://api.languagetool.org";

/// Raccourci par défaut. Choisi pour éviter les conflits courants :
/// - Windows : Ctrl+Maj+Espace = espace insécable dans Word, donc on l'évite ;
/// - macOS : Cmd+Option+Espace = recherche Finder, Ctrl+Espace = source de saisie.
pub fn default_shortcut() -> String {
    if cfg!(target_os = "macos") {
        "Cmd+Shift+Space".into()
    } else {
        "Ctrl+Alt+Space".into()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// URL de base du serveur LanguageTool (sans /v2).
    pub server_url: String,
    /// Code langue LanguageTool ("fr", "en-US", "auto"…).
    pub language: String,
    /// Raccourci global (format Tauri : "Ctrl+Alt+Space", "Cmd+Shift+Space"…).
    pub shortcut: String,
    /// Mode exigeant (règles de style supplémentaires).
    pub picky: bool,
    /// Vérification automatique pendant la frappe.
    pub live: bool,
    /// Restaure le contenu texte précédent du presse-papiers après un collage.
    pub restore_clipboard: bool,
    /// Mots ignorés par le correcteur orthographique.
    pub dictionary: Vec<String>,
    /// Identifiants de règles LanguageTool désactivées.
    pub disabled_rules: Vec<String>,
    /// Nom d'utilisateur LanguageTool Premium (optionnel).
    pub username: String,
    /// Clé API LanguageTool Premium (optionnelle).
    pub api_key: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server_url: PUBLIC_SERVER.into(),
            language: "fr".into(),
            shortcut: default_shortcut(),
            picky: false,
            live: true,
            restore_clipboard: true,
            dictionary: Vec::new(),
            disabled_rules: Vec::new(),
            username: String::new(),
            api_key: String::new(),
        }
    }
}

impl Config {
    pub fn load(path: &PathBuf) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<Config>(&s).ok())
            .map(Config::normalized)
            .unwrap_or_default()
    }

    pub fn save(&self, path: &PathBuf) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        // Écriture atomique : fichier temporaire puis renommage.
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json).map_err(|e| e.to_string())?;
        fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    /// Nettoie les champs saisis par l'utilisateur.
    pub fn normalized(mut self) -> Self {
        self.server_url = normalize_server_url(&self.server_url);
        self.language = self.language.trim().to_string();
        if self.language.is_empty() {
            self.language = "fr".into();
        }
        self.shortcut = self.shortcut.trim().to_string();
        if self.shortcut.is_empty() {
            self.shortcut = default_shortcut();
        }
        self.username = self.username.trim().to_string();
        self.api_key = self.api_key.trim().to_string();
        self.dictionary = dedup_trimmed(self.dictionary);
        self.disabled_rules = dedup_trimmed(self.disabled_rules);
        self
    }

    pub fn has_word(&self, word: &str) -> bool {
        let w = word.trim().to_lowercase();
        self.dictionary.iter().any(|d| d.to_lowercase() == w)
    }
}

fn dedup_trimmed(items: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for it in items {
        let t = it.trim();
        if !t.is_empty() && !out.iter().any(|o| o == t) {
            out.push(t.to_string());
        }
    }
    out
}

/// Accepte "monserveur:8010", "http://x/v2", "https://x/v2/check/"… et renvoie l'URL de base.
pub fn normalize_server_url(raw: &str) -> String {
    let mut url = raw.trim().trim_end_matches('/').to_string();
    if url.is_empty() {
        return PUBLIC_SERVER.into();
    }
    if !url.contains("://") {
        url = format!("http://{url}");
    }
    for suffix in ["/v2/check", "/v2/languages", "/v2"] {
        if let Some(stripped) = url.strip_suffix(suffix) {
            url = stripped.to_string();
            break;
        }
    }
    url.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_url_normalization() {
        assert_eq!(normalize_server_url(""), PUBLIC_SERVER);
        assert_eq!(normalize_server_url("lt.local:8010"), "http://lt.local:8010");
        assert_eq!(normalize_server_url("https://lt.example.org/v2/check/"), "https://lt.example.org");
        assert_eq!(normalize_server_url(" http://100.64.0.5:8010/v2 "), "http://100.64.0.5:8010");
        assert_eq!(normalize_server_url("https://lt.example.org/sub/"), "https://lt.example.org/sub");
    }

    #[test]
    fn config_roundtrip_and_defaults() {
        let partial: Config = serde_json::from_str(r#"{"language":"en-US"}"#).unwrap();
        assert_eq!(partial.language, "en-US");
        assert_eq!(partial.server_url, PUBLIC_SERVER);
        assert!(partial.live);
        let c = Config {
            dictionary: vec![" Proxmox ".into(), "Proxmox".into(), "".into()],
            ..Default::default()
        }
        .normalized();
        assert_eq!(c.dictionary, vec!["Proxmox".to_string()]);
        assert!(c.has_word("proxmox"));
    }
}
