// =============================================================================
// Orbis — Internationalization (i18n)
// =============================================================================
// Loads localized strings from JSON language files in assets/lang/.
//
// System language is auto-detected at startup. Falls back to English
// if no matching language file exists. Language can be changed at
// runtime via set_language().
//
// Usage:
//   i18n::init(None);                     // Auto-detect
//   i18n::init(Some("de"));               // Force German
//   let label = i18n::t("layers_heading"); // Returns localized string
//   i18n::set_language("fr");             // Switch at runtime
// =============================================================================

use std::collections::HashMap;
use std::sync::RwLock;

/// Global i18n instance, switchable at runtime.
static I18N: RwLock<Option<I18n>> = RwLock::new(None);

/// Holds the loaded translations for the active language.
struct I18n {
    strings: HashMap<String, String>,
    language_code: String,
}

/// Initialize the i18n system.
///
/// If `preferred` is Some, uses that language code.
/// Otherwise auto-detects the system language.
/// Falls back to English if the requested language is unavailable.
pub fn init(preferred: Option<&str>) {
    let code = match preferred {
        Some(c) => c.to_string(),
        None => detect_system_language(),
    };
    log::info!("i18n init: requested '{}'", code);
    load_and_set(&code);
}

/// Switch the active language at runtime.
///
/// Reloads strings from the corresponding JSON file.
/// Falls back to English if the requested language file is missing.
pub fn set_language(code: &str) {
    log::info!("i18n: switching to '{}'", code);
    load_and_set(code);
}

/// Returns the currently active language code (e.g. "en", "de").
pub fn current_language() -> String {
    match I18N.read().ok() {
        Some(guard) => match guard.as_ref() {
            Some(i18n) => i18n.language_code.clone(),
            None => "en".to_string(),
        },
        None => "en".to_string(),
    }
}

/// Look up a translated string by key.
///
/// Returns the localized string, or the key itself if not found.
pub fn t(key: &str) -> String {
    match I18N.read().ok() {
        Some(guard) => match guard.as_ref() {
            Some(i18n) => i18n
                .strings
                .get(key)
                .cloned()
                .unwrap_or_else(|| key.to_string()),
            None => key.to_string(),
        },
        None => key.to_string(),
    }
}

/// Look up a translated string and replace `{}` with the given value.
///
/// Example: `t_fmt("gibs_ready", "2026-02-28")` → "✅ GIBS: 2026-02-28"
#[allow(dead_code)]
pub fn t_fmt(key: &str, value: &str) -> String {
    t(key).replace("{}", value)
}

/// Scans assets/lang/ and returns all available languages as (code, name) pairs.
///
/// The name comes from `_meta.language` in each JSON file.
/// Results are sorted alphabetically by display name.
pub fn available_languages() -> Vec<(String, String)> {
    let lang_dir = crate::app_path("assets/lang");
    let mut languages = Vec::new();

    let entries = match std::fs::read_dir(&lang_dir) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("Could not scan language directory: {}", e);
            return vec![("en".to_string(), "English".to_string())];
        }
    };

    // RTL languages excluded until egui gains bidirectional text support
    // (see https://github.com/emilk/egui/issues/1016)
    const EXCLUDED: &[&str] = &["ar"];

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map_or(false, |ext| ext == "json") {
            let code = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            if EXCLUDED.contains(&code.as_str()) {
                continue;
            }

            // Read display name from _meta.language
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(raw) = serde_json::from_str::<HashMap<String, serde_json::Value>>(&content) {
                    let name = raw
                        .get("_meta")
                        .and_then(|m| m.get("language"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(&code)
                        .to_string();
                    languages.push((code, name));
                }
            }
        }
    }

    languages.sort_by(|a, b| a.1.cmp(&b.1));
    languages
}

// =============================================================================
// Internal
// =============================================================================

/// Loads a language file and sets it as active.
fn load_and_set(code: &str) {
    let codes_to_try = if code == "en" {
        vec!["en".to_string()]
    } else {
        vec![code.to_string(), "en".to_string()]
    };

    for c in &codes_to_try {
        match load_language_file(c) {
            Ok(strings) => {
                log::info!("Language loaded: {} ({} strings)", c, strings.len());
                if let Ok(mut guard) = I18N.write() {
                    *guard = Some(I18n {
                        strings,
                        language_code: c.clone(),
                    });
                }
                return;
            }
            Err(e) => {
                log::warn!("Failed to load language '{}': {}", c, e);
            }
        }
    }

    // Ultimate fallback: empty map
    log::error!("No language files found, using raw keys");
    if let Ok(mut guard) = I18N.write() {
        *guard = Some(I18n {
            strings: HashMap::new(),
            language_code: "en".to_string(),
        });
    }
}

/// Detect the system language from environment / OS locale.
fn detect_system_language() -> String {
    for var in &["LANG", "LC_ALL", "LC_MESSAGES", "LANGUAGE"] {
        if let Ok(val) = std::env::var(var) {
            if let Some(code) = parse_locale_code(&val) {
                return code;
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(code) = detect_windows_language() {
            return code;
        }
    }

    "en".to_string()
}

/// Parse a locale string like "de_DE.UTF-8" into a 2-letter code.
fn parse_locale_code(locale: &str) -> Option<String> {
    let locale = locale.trim();
    if locale.is_empty() || locale == "C" || locale == "POSIX" {
        return None;
    }
    let code: String = locale
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .take(2)
        .collect::<String>()
        .to_lowercase();
    if code.len() == 2 { Some(code) } else { None }
}

/// Windows-specific language detection.
#[cfg(target_os = "windows")]
fn detect_windows_language() -> Option<String> {
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", "(Get-Culture).TwoLetterISOLanguageName"])
        .output()
        .ok()?;
    if output.status.success() {
        let code = String::from_utf8_lossy(&output.stdout).trim().to_lowercase();
        if code.len() == 2 && code.chars().all(|c| c.is_ascii_alphabetic()) {
            return Some(code);
        }
    }
    None
}

/// Load a language file from assets/lang/{code}.json.
fn load_language_file(code: &str) -> Result<HashMap<String, String>, String> {
    let path = crate::app_path(&format!("assets/lang/{}.json", code));
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("{}: {}", path.display(), e))?;
    let raw: HashMap<String, serde_json::Value> = serde_json::from_str(&content)
        .map_err(|e| format!("JSON parse error in {}: {}", path.display(), e))?;

    let strings: HashMap<String, String> = raw
        .into_iter()
        .filter(|(k, _)| !k.starts_with('_'))
        .filter_map(|(k, v)| {
            if let serde_json::Value::String(s) = v {
                Some((k, s))
            } else {
                None
            }
        })
        .collect();

    Ok(strings)
}
