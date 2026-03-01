use clap::Command;
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

static ACTIVE_I18N: OnceLock<I18n> = OnceLock::new();

type Catalog = BTreeMap<String, String>;

pub fn init_from_cli_args(raw_args: &[String]) {
    let cli_locale = extract_locale_arg(raw_args);
    let _ = ACTIVE_I18N.set(I18n::load(cli_locale));
}

pub fn t(key: &str) -> String {
    ACTIVE_I18N.get_or_init(|| I18n::load(None)).t(key)
}

pub fn tf(key: &str, args: &[(&str, String)]) -> String {
    ACTIVE_I18N.get_or_init(|| I18n::load(None)).tf(key, args)
}

pub fn apply_localized_help(mut cmd: Command) -> Command {
    cmd = cmd
        .about(t("cli.meta.about"))
        .long_about(t("cli.meta.long_about"))
        .mut_arg("locale", |a| a.help(t("cli.help.wizard.locale")));
    cmd = cmd.mut_subcommand("wizard", |sc| {
        sc.about(t("cli.help.wizard.about"))
            .mut_arg("spec", |a| a.help(t("cli.help.wizard.spec")))
            .mut_arg("answers", |a| a.help(t("cli.help.wizard.answers")))
            .mut_arg("verbose", |a| a.help(t("cli.help.wizard.verbose")))
            .mut_arg("answers_json", |a| {
                a.help(t("cli.help.wizard.answers_json"))
            })
            .mut_arg("format", |a| a.help(t("cli.help.wizard.format")))
            .mut_arg("i18n_resolved", |a| {
                a.help(t("cli.help.wizard.i18n_resolved"))
            })
            .mut_arg("i18n_debug", |a| a.help(t("cli.help.wizard.i18n_debug")))
    });
    cmd = cmd.mut_subcommand("new", |sc| {
        sc.about(t("cli.help.new.about"))
            .mut_arg("out", |a| a.help(t("cli.help.new.out")))
            .mut_arg("force", |a| a.help(t("cli.help.new.force")))
            .mut_arg("verbose", |a| a.help(t("cli.help.new.verbose")))
    });
    cmd = cmd.mut_subcommand("generate", |sc| {
        sc.about(t("cli.help.generate.about"))
            .mut_arg("input", |a| a.help(t("cli.help.generate.input")))
            .mut_arg("out", |a| a.help(t("cli.help.generate.out")))
            .mut_arg("force", |a| a.help(t("cli.help.generate.force")))
            .mut_arg("verbose", |a| a.help(t("cli.help.generate.verbose")))
    });
    cmd.mut_subcommand("validate", |sc| {
        sc.about(t("cli.help.validate.about"))
            .mut_arg("spec", |a| a.help(t("cli.help.validate.spec")))
            .mut_arg("answers", |a| a.help(t("cli.help.validate.answers")))
    })
}

fn extract_locale_arg(raw_args: &[String]) -> Option<String> {
    let mut iter = raw_args.iter();
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix("--locale=") {
            return Some(value.to_string());
        }
        if arg == "--locale"
            && let Some(value) = iter.next()
        {
            return Some(value.to_string());
        }
    }
    None
}

#[derive(Debug)]
struct I18n {
    active: Catalog,
    english: Catalog,
}

impl I18n {
    fn load(cli_locale: Option<String>) -> Self {
        let supported = supported_locales();
        let selected = select_locale(cli_locale, &supported);
        let english = load_catalog("en").unwrap_or_default();
        let active = load_catalog(&selected).unwrap_or_else(|| english.clone());
        Self { active, english }
    }

    fn t(&self, key: &str) -> String {
        self.active
            .get(key)
            .or_else(|| self.english.get(key))
            .cloned()
            .unwrap_or_else(|| key.to_string())
    }

    fn tf(&self, key: &str, args: &[(&str, String)]) -> String {
        let mut rendered = self.t(key);
        for (name, value) in args {
            let token = format!("{{{}}}", name);
            rendered = rendered.replace(&token, value);
        }
        rendered
    }
}

fn supported_locales() -> Vec<String> {
    let mut locales = [
        "ar", "ar-AE", "ar-DZ", "ar-EG", "ar-IQ", "ar-MA", "ar-SA", "ar-SD", "ar-SY", "ar-TN",
        "ay", "bg", "bn", "cs", "da", "de", "el", "en", "en-GB", "es", "et", "fa", "fi", "fr",
        "gn", "gu", "hi", "hr", "ht", "hu", "id", "it", "ja", "km", "kn", "ko", "lo", "lt", "lv",
        "ml", "mr", "ms", "my", "nah", "ne", "nl", "no", "pa", "pl", "pt", "qu", "ro", "ru", "si",
        "sk", "sr", "sv", "ta", "te", "th", "tl", "tr", "uk", "ur", "vi", "zh",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect::<Vec<_>>();
    locales.sort();
    locales
}

fn load_catalog(locale: &str) -> Option<Catalog> {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("i18n");
    path.push(format!("{}.json", locale));
    let text = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let object = value.as_object()?;
    let mut map = Catalog::new();
    for (key, value) in object {
        if let Some(text) = value.as_str() {
            map.insert(key.clone(), text.to_string());
        }
    }
    Some(map)
}

fn detect_env_locale() -> Option<String> {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(val) = env::var(key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn detect_system_locale() -> Option<String> {
    None
}

fn normalize_locale(raw: &str) -> Option<String> {
    let mut cleaned = raw.trim();
    if cleaned.is_empty() {
        return None;
    }
    if let Some((head, _)) = cleaned.split_once('.') {
        cleaned = head;
    }
    if let Some((head, _)) = cleaned.split_once('@') {
        cleaned = head;
    }
    let cleaned = cleaned.replace('_', "-");
    if cleaned
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        Some(cleaned)
    } else {
        None
    }
}

fn base_language(tag: &str) -> Option<String> {
    tag.split('-').next().map(|s| s.to_ascii_lowercase())
}

fn select_locale(cli_locale: Option<String>, supported: &[String]) -> String {
    fn resolve(candidate: &str, supported: &[String]) -> Option<String> {
        let norm = normalize_locale(candidate)?;
        if supported.iter().any(|s| s == &norm) {
            return Some(norm);
        }
        let base = base_language(&norm)?;
        if supported.iter().any(|s| s == &base) {
            return Some(base);
        }
        None
    }

    if let Some(cli) = cli_locale.as_deref()
        && let Some(found) = resolve(cli, supported)
    {
        return found;
    }

    if let Some(env_loc) = detect_env_locale()
        && let Some(found) = resolve(&env_loc, supported)
    {
        return found;
    }

    if let Some(sys_loc) = detect_system_locale()
        && let Some(found) = resolve(&sys_loc, supported)
    {
        return found;
    }

    "en".to_string()
}
