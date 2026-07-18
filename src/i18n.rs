//! Built-in i18n catalogs (embedded from repo `languages/*.lang`).
//!
//! Resolution order for active language:
//! 1. `config.language` when set and a catalog exists
//! 2. system locale mapped to a known catalog (e.g. `zh*` → `zh_cn`)
//! 3. `en_us` (English is the built-in base; missing keys always fall back to English)
//!
//! Catalogs are compiled into the binary via `include_str!` so a single exe ships
//! without external language files. The `languages/` folder is the editable source.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

pub const LANG_EN: &str = "en_us";
pub const LANG_ZH_CN: &str = "zh_cn";

const EMBEDDED_EN_US: &str = include_str!("../languages/en_us.lang");
const EMBEDDED_ZH_CN: &str = include_str!("../languages/zh_cn.lang");

/// One language catalog (key → value).
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub code: String,
    map: HashMap<String, String>,
}

impl Catalog {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(|s| s.as_str())
    }

    pub fn native_name(&self) -> String {
        self.get("meta.native_name")
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.code.clone())
    }
}

/// Parse UTF-8 `key=value` language file text.
/// Supports `#` comments, blank lines, and `\n` escape in values.
pub fn parse_lang(text: &str) -> Catalog {
    let mut map = HashMap::new();
    let mut code = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim().to_string();
        if key.is_empty() {
            continue;
        }
        let value = unescape_value(v.trim());
        if key == "meta.code" {
            code = value.clone();
        }
        map.insert(key, value);
    }
    if code.is_empty() {
        code = map
            .get("meta.code")
            .cloned()
            .unwrap_or_else(|| "unknown".into());
    }
    Catalog { code, map }
}

fn unescape_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn apply_args(template: &str, args: &[(&str, &str)]) -> String {
    let mut s = template.to_string();
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

struct I18nState {
    active_code: String,
    active: Catalog,
    english: Catalog,
    /// All embedded catalogs by code.
    all: HashMap<String, Catalog>,
}

impl I18nState {
    fn new() -> Self {
        let english = parse_lang(EMBEDDED_EN_US);
        let mut zh = parse_lang(EMBEDDED_ZH_CN);
        if zh.code.trim().is_empty() {
            zh.code = LANG_ZH_CN.to_string();
        }
        let mut all = HashMap::new();
        all.insert(LANG_EN.to_string(), english.clone());
        all.insert(zh.code.clone(), zh);
        Self {
            active_code: LANG_EN.to_string(),
            active: english.clone(),
            english,
            all,
        }
    }

    fn translate(&self, key: &str) -> String {
        if let Some(v) = self.active.get(key) {
            return v.to_string();
        }
        if let Some(v) = self.english.get(key) {
            return v.to_string();
        }
        key.to_string()
    }
}

fn state() -> &'static RwLock<I18nState> {
    static STATE: OnceLock<RwLock<I18nState>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(I18nState::new()))
}

/// Codes of all embedded catalogs (sorted), for Settings dropdown.
pub fn available_languages() -> Vec<(String, String)> {
    let st = state().read().expect("i18n lock");
    let mut items: Vec<(String, String)> = st
        .all
        .iter()
        .map(|(code, cat)| (code.clone(), cat.native_name()))
        .collect();
    items.sort_by(|a, b| a.0.cmp(&b.0));
    items
}

pub fn active_language() -> String {
    state()
        .read()
        .expect("i18n lock")
        .active_code
        .clone()
}

/// Switch active catalog. Returns false if code is unknown (stays on previous).
pub fn set_language(code: &str) -> bool {
    let code = normalize_lang_code(code);
    let mut st = state().write().expect("i18n lock");
    if let Some(cat) = st.all.get(&code).cloned() {
        st.active_code = code;
        st.active = cat;
        true
    } else {
        false
    }
}

/// Lookup with English fallback (then key).
pub fn t(key: &str) -> String {
    state().read().expect("i18n lock").translate(key)
}

/// Lookup + `{name}` substitution.
pub fn t_args(key: &str, args: &[(&str, &str)]) -> String {
    let base = t(key);
    apply_args(&base, args)
}

/// Whether an embedded catalog exists for this code.
pub fn has_language(code: &str) -> bool {
    let code = normalize_lang_code(code);
    state().read().expect("i18n lock").all.contains_key(&code)
}

pub fn normalize_lang_code(code: &str) -> String {
    let s = code.trim().to_lowercase().replace('-', "_");
    match s.as_str() {
        "zh" | "zh_cn" | "zh_hans" | "zh_sg" | "chs" | "chinese" => LANG_ZH_CN.into(),
        "en" | "en_us" | "en_gb" | "en_au" | "english" => LANG_EN.into(),
        other if other.is_empty() => LANG_EN.into(),
        other => other.to_string(),
    }
}

/// Map a system locale string (e.g. `zh-CN`, `en_US.UTF-8`) to a catalog code if known.
pub fn locale_to_lang_code(locale: &str) -> Option<String> {
    let raw = locale.trim();
    if raw.is_empty() || raw == "C" || raw == "POSIX" {
        return None;
    }
    // Strip encoding: en_US.UTF-8 → en_US
    let base = raw.split('.').next().unwrap_or(raw);
    let base = base.split('@').next().unwrap_or(base);
    let norm = normalize_lang_code(base);
    if has_language(&norm) {
        Some(norm)
    } else {
        // Try primary subtag only
        let primary = base
            .split(['_', '-'])
            .next()
            .unwrap_or(base);
        let norm2 = normalize_lang_code(primary);
        if has_language(&norm2) {
            Some(norm2)
        } else {
            None
        }
    }
}

/// Best-effort system UI locale (env first, then Windows API).
pub fn system_locale_string() -> Option<String> {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(key) {
            let v = v.trim().to_string();
            if !v.is_empty() && v != "C" && v != "POSIX" {
                return Some(v);
            }
        }
    }
    #[cfg(windows)]
    {
        if let Some(s) = windows_locale_name() {
            return Some(s);
        }
    }
    None
}

#[cfg(windows)]
fn windows_locale_name() -> Option<String> {
    use std::os::windows::ffi::OsStringExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultLocaleName(lpLocaleName: *mut u16, cchLocaleName: i32) -> i32;
    }
    let mut buf = [0u16; 85];
    let n = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
    if n <= 1 {
        return None;
    }
    let os = std::ffi::OsString::from_wide(&buf[..(n as usize - 1)]);
    let s = os.to_string_lossy().into_owned();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Startup resolution: config language → system locale → English.
pub fn resolve_startup_language(config_language: &str) -> String {
    let cfg = config_language.trim();
    if !cfg.is_empty() {
        let code = normalize_lang_code(cfg);
        if has_language(&code) {
            return code;
        }
    }
    if let Some(loc) = system_locale_string() {
        if let Some(code) = locale_to_lang_code(&loc) {
            return code;
        }
    }
    LANG_EN.to_string()
}

/// Apply resolved language at process start (idempotent).
pub fn init_from_config(config_language: &str) -> String {
    let code = resolve_startup_language(config_language);
    let _ = set_language(&code);
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_value_and_escape() {
        let cat = parse_lang(
            r#"
# comment
meta.code=test
meta.native_name=Test
hello=Hello
multi=line\nbreak
"#,
        );
        assert_eq!(cat.code, "test");
        assert_eq!(cat.get("hello"), Some("Hello"));
        assert_eq!(cat.get("multi"), Some("line\nbreak"));
        assert_eq!(cat.native_name(), "Test");
    }

    #[test]
    fn embedded_en_and_zh_load() {
        let en = parse_lang(EMBEDDED_EN_US);
        let zh = parse_lang(EMBEDDED_ZH_CN);
        assert_eq!(en.code, LANG_EN);
        assert_eq!(zh.code, LANG_ZH_CN);
        assert!(en.get("tab.settings").is_some());
        assert!(zh.get("tab.settings").is_some());
        assert_ne!(en.get("tab.settings"), zh.get("tab.settings"));
    }

    #[test]
    fn missing_key_falls_back_to_english() {
        // Active may be zh; invent a key only in English path via t after ensure en has key.
        let _ = set_language(LANG_ZH_CN);
        let en_val = parse_lang(EMBEDDED_EN_US)
            .get("settings.save")
            .unwrap()
            .to_string();
        // unknown key → key itself after EN miss
        let missing = t("this.key.does.not.exist.anywhere");
        assert_eq!(missing, "this.key.does.not.exist.anywhere");
        // known EN key exists when on zh if zh missing would fall back — both have settings.save
        let _ = set_language(LANG_EN);
        assert_eq!(t("settings.save"), en_val);
    }

    #[test]
    fn fallback_uses_english_when_zh_missing_key() {
        // Inject: translate path uses english when active lacks key.
        // We simulate by looking up a key present only in EN catalog structure:
        // all shipped keys exist in both; test translate logic via Catalog directly.
        let mut active = parse_lang("meta.code=xx\nonly_xx=XX\n");
        active.code = "xx".into();
        let english = parse_lang(EMBEDDED_EN_US);
        let key = "settings.save";
        assert!(english.get(key).is_some());
        assert!(active.get(key).is_none());
        let resolved = active
            .get(key)
            .or_else(|| english.get(key))
            .unwrap();
        assert_eq!(resolved, english.get(key).unwrap());
    }

    #[test]
    fn t_args_replaces_placeholders() {
        let _ = set_language(LANG_EN);
        let s = t_args("app.local_device", &[("name", "PC1")]);
        assert!(s.contains("PC1"), "{s}");
        assert!(!s.contains("{name}"), "{s}");
    }

    #[test]
    fn locale_map_zh_and_en() {
        assert_eq!(locale_to_lang_code("zh-CN").as_deref(), Some(LANG_ZH_CN));
        assert_eq!(locale_to_lang_code("zh_CN.UTF-8").as_deref(), Some(LANG_ZH_CN));
        assert_eq!(locale_to_lang_code("en_US").as_deref(), Some(LANG_EN));
        assert_eq!(locale_to_lang_code("en-GB").as_deref(), Some(LANG_EN));
        assert_eq!(locale_to_lang_code("C"), None);
        assert_eq!(locale_to_lang_code(""), None);
    }

    #[test]
    fn resolve_prefers_config_then_falls_to_en() {
        let code = resolve_startup_language("zh_cn");
        assert_eq!(code, LANG_ZH_CN);
        let code = resolve_startup_language("not_a_lang");
        // may be system locale or en
        assert!(has_language(&code));
        let code = resolve_startup_language("");
        assert!(has_language(&code));
    }

    #[test]
    fn available_lists_embedded() {
        let list = available_languages();
        let codes: Vec<_> = list.iter().map(|(c, _)| c.as_str()).collect();
        assert!(codes.contains(&LANG_EN));
        assert!(codes.contains(&LANG_ZH_CN));
    }

    #[test]
    fn set_language_switches_catalog() {
        assert!(set_language(LANG_ZH_CN));
        assert_eq!(active_language(), LANG_ZH_CN);
        let zh_save = t("settings.save");
        assert!(set_language(LANG_EN));
        let en_save = t("settings.save");
        assert_ne!(zh_save, en_save);
        assert!(!set_language("nope_lang"));
        assert_eq!(active_language(), LANG_EN);
    }
}
