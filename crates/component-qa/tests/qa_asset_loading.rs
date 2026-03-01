use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use component_qa::qa::{NormalizedMode, qa_spec_json};
use serde_json::{Value, json};
use tempfile::TempDir;

const MISSING_CONFIG_MESSAGE: &str =
    "No QA form configured. Create one with `greentic-qa new` and reference its asset path.";

fn env_lock() -> &'static Mutex<()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK.get_or_init(|| Mutex::new(()))
}

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    match env_lock().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn copy_dir_recursive(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create dir");
    for entry in std::fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("entry");
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_dir_recursive(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).expect("copy file");
        }
    }
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/generated")
}

fn setup_generated_assets() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let qa_dir = dir.path().join("qa");
    copy_dir_recursive(&fixture_root().join("forms"), &qa_dir.join("forms"));
    copy_dir_recursive(&fixture_root().join("i18n"), &qa_dir.join("i18n"));
    dir
}

#[test]
fn qa_form_asset_path_loads_real_form_instead_of_placeholder() {
    let _guard = lock_env();
    let assets = setup_generated_assets();
    // Guarded by process-wide mutex to avoid concurrent env mutation across tests.
    unsafe { std::env::set_var("QA_FORM_ASSET_BASE", assets.path()) };

    let payload = json!({
        "form_id": "support-form",
        "config": {
            "qa_form_asset_path": "qa/forms/support.form.json"
        },
        "ctx": {
            "locale": "en"
        }
    });
    let spec = qa_spec_json(NormalizedMode::Setup, &payload);

    let questions = spec
        .get("questions")
        .and_then(Value::as_array)
        .expect("questions array");
    assert_eq!(questions.len(), 2);
    assert_eq!(
        questions[0].get("id").and_then(Value::as_str),
        Some("api_key")
    );
    assert_eq!(
        questions[1].get("id").and_then(Value::as_str),
        Some("enabled")
    );
}

#[test]
fn missing_qa_form_asset_path_fails_fast_with_guidance() {
    let payload = json!({
        "form_id": "support-form"
    });
    let spec = qa_spec_json(NormalizedMode::Setup, &payload);
    assert_eq!(
        spec.pointer("/description/default").and_then(Value::as_str),
        Some(MISSING_CONFIG_MESSAGE)
    );
    assert_eq!(
        spec.pointer("/questions")
            .and_then(Value::as_array)
            .map(|list| list.len()),
        Some(0)
    );
}

#[test]
fn missing_i18n_keys_in_en_are_reported_deterministically() {
    let _guard = lock_env();
    let assets = setup_generated_assets();
    // Guarded by process-wide mutex to avoid concurrent env mutation across tests.
    unsafe { std::env::set_var("QA_FORM_ASSET_BASE", assets.path()) };

    let en_path = assets.path().join("qa").join("i18n").join("en.json");
    std::fs::write(
        &en_path,
        r#"{
  "qa.form.support.field.api_key.label": "API key"
}"#,
    )
    .expect("write en fixture");

    let payload = json!({
        "form_id": "support-form",
        "config": {
            "qa_form_asset_path": "qa/forms/support.form.json"
        },
        "ctx": {
            "locale": "en"
        }
    });
    let spec = qa_spec_json(NormalizedMode::Setup, &payload);
    let description = spec
        .pointer("/description/default")
        .and_then(Value::as_str)
        .expect("error description");

    assert!(description.contains("references i18n keys missing"));
    assert!(description.contains("qa.form.support.field.api_key.help"));
    assert!(description.contains("qa.form.support.field.enabled.help"));
    assert!(description.contains("qa.form.support.field.enabled.label"));
}

#[test]
fn golden_generated_form_uses_locale_and_en_fallback() {
    let _guard = lock_env();
    let assets = setup_generated_assets();
    // Guarded by process-wide mutex to avoid concurrent env mutation across tests.
    unsafe { std::env::set_var("QA_FORM_ASSET_BASE", assets.path()) };

    let payload = json!({
        "form_id": "support-form",
        "config": {
            "qa_form_asset_path": "qa/forms/support.form.json"
        },
        "ctx": {
            "locale": "nl-NL"
        }
    });
    let spec = qa_spec_json(NormalizedMode::Setup, &payload);
    let questions = spec
        .get("questions")
        .and_then(Value::as_array)
        .expect("questions array");

    assert_eq!(
        questions[0]
            .pointer("/label/fallback")
            .and_then(Value::as_str),
        Some("API-sleutel")
    );
    assert_eq!(
        questions[1]
            .pointer("/label/fallback")
            .and_then(Value::as_str),
        Some("Provider inschakelen")
    );
    assert_eq!(
        questions[0]
            .pointer("/help/fallback")
            .and_then(Value::as_str),
        Some("Secret key for provider auth")
    );
    assert_eq!(
        questions[1]
            .pointer("/help/fallback")
            .and_then(Value::as_str),
        Some("Enable after setup")
    );
}
