use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use greentic_types::i18n_text::I18nText;
use greentic_types::schemas::component::v0_6_0::{
    ChoiceOption, ComponentQaSpec, QaMode, Question, QuestionKind,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use thiserror::Error;

use qa_spec::{
    FormSpec, ProgressContext, QuestionType, RenderPayload, StoreContext, StoreError, StoreOp,
    VisibilityMode, answers_schema, build_render_payload, example_answers, next_question,
    render_card as qa_render_card, render_json_ui as qa_render_json_ui,
    render_text as qa_render_text, resolve_visibility, validate,
};

const MISSING_QA_FORM_CONFIG_MESSAGE: &str =
    "No QA form configured. Create one with `greentic-qa new` and reference its asset path.";

#[derive(Debug, Error)]
enum ComponentError {
    #[error("failed to parse config/{0}")]
    ConfigParse(#[source] serde_json::Error),
    #[error("{MISSING_QA_FORM_CONFIG_MESSAGE}")]
    MissingQaFormAssetPath,
    #[error("failed to read QA form asset; path='{path}'; details: {source}")]
    QaFormRead {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse QA form asset '{path}': {source}")]
    QaFormParse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to read i18n locale file '{path}': {source}")]
    I18nRead {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse i18n locale file '{path}': {source}")]
    I18nParse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("missing i18n baseline file 'en.json' for QA form '{form_path}' under '{i18n_dir}'")]
    MissingI18nEnglish { form_path: String, i18n_dir: String },
    #[error(
        "QA form '{form_path}' references i18n keys missing from '{i18n_en_path}': {missing_keys}"
    )]
    MissingI18nKeys {
        form_path: String,
        i18n_en_path: String,
        missing_keys: String,
    },
    #[error("form '{0}' is not available")]
    FormUnavailable(String),
    #[error("json encode error: {0}")]
    JsonEncode(#[source] serde_json::Error),
    #[error("include expansion failed: {0}")]
    Include(String),
    #[error("store apply failed: {0}")]
    Store(#[from] StoreError),
}

#[derive(Debug, Deserialize, Serialize, Default)]
struct ComponentConfig {
    #[serde(default)]
    qa_form_asset_path: Option<String>,
    #[serde(default)]
    include_registry: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct LoadedFormValue {
    spec_value: Value,
    form_asset_path: String,
}

fn load_form_spec(config_json: &str) -> Result<FormSpec, ComponentError> {
    let loaded = load_form_spec_value(config_json)?;
    let spec: FormSpec =
        serde_json::from_value(loaded.spec_value).map_err(ComponentError::ConfigParse)?;
    validate_form_i18n_keys(&spec, &loaded.form_asset_path)?;
    Ok(spec)
}

fn load_form_spec_value(config_json: &str) -> Result<LoadedFormValue, ComponentError> {
    if config_json.trim().is_empty() {
        return Err(ComponentError::MissingQaFormAssetPath);
    }
    let parsed: Value = serde_json::from_str(config_json).map_err(ComponentError::ConfigParse)?;
    let config: ComponentConfig =
        serde_json::from_value(parsed).map_err(ComponentError::ConfigParse)?;
    let qa_form_asset_path = config
        .qa_form_asset_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or(ComponentError::MissingQaFormAssetPath)?;
    let (raw_spec, resolved_path) = read_qa_form_asset(qa_form_asset_path)?;
    let mut spec_value: Value =
        serde_json::from_str(&raw_spec).map_err(|source| ComponentError::QaFormParse {
            path: resolved_path.clone(),
            source,
        })?;
    let include_registry_values = parse_include_registry(config.include_registry)?;
    if !include_registry_values.is_empty() {
        spec_value = expand_includes_value(&spec_value, &include_registry_values)?;
    }
    Ok(LoadedFormValue {
        spec_value,
        form_asset_path: resolved_path,
    })
}

fn parse_include_registry(
    include_registry: BTreeMap<String, String>,
) -> Result<BTreeMap<String, Value>, ComponentError> {
    let mut registry = BTreeMap::new();
    for (form_ref, raw_form) in include_registry {
        let value = serde_json::from_str(&raw_form).map_err(ComponentError::ConfigParse)?;
        registry.insert(form_ref, value);
    }
    Ok(registry)
}

fn qa_asset_base_path() -> String {
    std::env::var("QA_FORM_ASSET_BASE").unwrap_or_else(|_| "assets".to_string())
}

fn candidate_form_paths(path: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    let mut push = |candidate: String| {
        if seen.insert(candidate.clone()) {
            candidates.push(candidate);
        }
    };

    if Path::new(path).is_absolute() {
        push(path.to_string());
        return candidates;
    }

    let base = qa_asset_base_path();
    push(
        PathBuf::from(&base)
            .join(path)
            .to_string_lossy()
            .to_string(),
    );
    push(path.to_string());
    push(
        PathBuf::from("/assets")
            .join(path)
            .to_string_lossy()
            .to_string(),
    );
    candidates
}

fn read_qa_form_asset(path: &str) -> Result<(String, String), ComponentError> {
    let candidates = candidate_form_paths(path);
    let mut last_read_error: Option<(String, std::io::Error)> = None;

    for candidate in candidates {
        match std::fs::read_to_string(&candidate) {
            Ok(contents) => return Ok((contents, candidate)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                last_read_error = Some((candidate, err))
            }
            Err(err) => {
                return Err(ComponentError::QaFormRead {
                    path: candidate,
                    source: err,
                });
            }
        }
    }

    if let Some((path, source)) = last_read_error {
        return Err(ComponentError::QaFormRead { path, source });
    }

    Err(ComponentError::MissingQaFormAssetPath)
}

fn infer_i18n_dir_from_form_path(form_asset_path: &str) -> String {
    let form_path = PathBuf::from(form_asset_path);
    let parts = form_path
        .iter()
        .map(|part| part.to_string_lossy().to_string())
        .collect::<Vec<_>>();

    if let Some(forms_pos) = parts.iter().position(|part| part == "forms") {
        let mut prefix = PathBuf::new();
        for part in &parts[..forms_pos] {
            prefix.push(part);
        }
        prefix.push("i18n");
        return prefix.to_string_lossy().to_string();
    }

    if let Some(parent) = form_path.parent()
        && parent.file_name().and_then(|name| name.to_str()) == Some("forms")
    {
        return parent
            .parent()
            .map(|base| base.join("i18n"))
            .unwrap_or_else(|| PathBuf::from("i18n"))
            .to_string_lossy()
            .to_string();
    }

    form_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("i18n")
        .to_string_lossy()
        .to_string()
}

fn load_locale_map(
    i18n_dir: &str,
    locale: &str,
) -> Result<Option<BTreeMap<String, String>>, ComponentError> {
    let path = PathBuf::from(i18n_dir).join(format!("{locale}.json"));
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path).map_err(|source| ComponentError::I18nRead {
        path: path.to_string_lossy().to_string(),
        source,
    })?;
    let parsed: BTreeMap<String, String> =
        serde_json::from_str(&raw).map_err(|source| ComponentError::I18nParse {
            path: path.to_string_lossy().to_string(),
            source,
        })?;
    Ok(Some(parsed))
}

fn collect_question_i18n_keys(question: &qa_spec::QuestionSpec, keys: &mut BTreeSet<String>) {
    if let Some(text) = &question.title_i18n {
        keys.insert(text.key.clone());
    }
    if let Some(text) = &question.description_i18n {
        keys.insert(text.key.clone());
    }
    if let Some(list) = &question.list {
        for field in &list.fields {
            collect_question_i18n_keys(field, keys);
        }
    }
}

fn validate_form_i18n_keys(spec: &FormSpec, form_asset_path: &str) -> Result<(), ComponentError> {
    let mut keys = BTreeSet::new();
    for question in &spec.questions {
        collect_question_i18n_keys(question, &mut keys);
    }
    if keys.is_empty() {
        return Ok(());
    }

    let i18n_dir = infer_i18n_dir_from_form_path(form_asset_path);
    let en =
        load_locale_map(&i18n_dir, "en")?.ok_or_else(|| ComponentError::MissingI18nEnglish {
            form_path: form_asset_path.to_string(),
            i18n_dir: i18n_dir.clone(),
        })?;

    let missing = keys
        .into_iter()
        .filter(|key| !en.contains_key(key))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }

    Err(ComponentError::MissingI18nKeys {
        form_path: form_asset_path.to_string(),
        i18n_en_path: PathBuf::from(i18n_dir)
            .join("en.json")
            .to_string_lossy()
            .to_string(),
        missing_keys: missing.join(", "),
    })
}

fn expand_includes_value(
    root: &Value,
    registry: &BTreeMap<String, Value>,
) -> Result<Value, ComponentError> {
    let mut chain = Vec::new();
    let mut seen_ids = BTreeSet::new();
    expand_form_value(root, "", registry, &mut chain, &mut seen_ids)
}

fn expand_form_value(
    form: &Value,
    prefix: &str,
    registry: &BTreeMap<String, Value>,
    chain: &mut Vec<String>,
    seen_ids: &mut BTreeSet<String>,
) -> Result<Value, ComponentError> {
    let form_obj = form
        .as_object()
        .ok_or_else(|| ComponentError::Include("form spec must be a JSON object".into()))?;
    let form_id = form_obj
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>")
        .to_string();
    if chain.contains(&form_id) {
        let pos = chain.iter().position(|id| id == &form_id).unwrap_or(0);
        let mut cycle = chain[pos..].to_vec();
        cycle.push(form_id);
        return Err(ComponentError::Include(format!(
            "include cycle detected: {:?}",
            cycle
        )));
    }
    chain.push(form_id);

    let mut out = form_obj.clone();
    out.insert("includes".into(), Value::Array(Vec::new()));
    out.insert("questions".into(), Value::Array(Vec::new()));
    out.insert("validations".into(), Value::Array(Vec::new()));

    let mut out_questions = Vec::new();
    let mut out_validations = Vec::new();

    for question in form_obj
        .get("questions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let mut q = question;
        prefix_question_value(&mut q, prefix);
        if let Some(id) = q.get("id").and_then(Value::as_str)
            && !seen_ids.insert(id.to_string())
        {
            return Err(ComponentError::Include(format!(
                "duplicate question id after include expansion: '{}'",
                id
            )));
        }
        out_questions.push(q);
    }

    for validation in form_obj
        .get("validations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let mut v = validation;
        prefix_validation_value(&mut v, prefix);
        out_validations.push(v);
    }

    for include in form_obj
        .get("includes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let form_ref = include
            .get("form_ref")
            .and_then(Value::as_str)
            .ok_or_else(|| ComponentError::Include("include missing form_ref".into()))?;
        let include_prefix = include.get("prefix").and_then(Value::as_str);
        let child_prefix = combine_prefix(prefix, include_prefix);
        let included = registry.get(form_ref).ok_or_else(|| {
            ComponentError::Include(format!("missing include target '{}'", form_ref))
        })?;
        let expanded = expand_form_value(included, &child_prefix, registry, chain, seen_ids)?;
        out_questions.extend(
            expanded
                .get("questions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        out_validations.extend(
            expanded
                .get("validations")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
    }

    out.insert("questions".into(), Value::Array(out_questions));
    out.insert("validations".into(), Value::Array(out_validations));
    chain.pop();

    Ok(Value::Object(out))
}

fn parse_context(ctx_json: &str) -> Value {
    serde_json::from_str(ctx_json).unwrap_or_else(|_| Value::Object(Map::new()))
}

fn parse_runtime_context(ctx_json: &str) -> Value {
    let parsed = parse_context(ctx_json);
    parsed
        .get("ctx")
        .and_then(Value::as_object)
        .map(|ctx| Value::Object(ctx.clone()))
        .unwrap_or(parsed)
}

fn combine_prefix(parent: &str, child: Option<&str>) -> String {
    match (parent.is_empty(), child.unwrap_or("").is_empty()) {
        (true, true) => String::new(),
        (false, true) => parent.to_string(),
        (true, false) => child.unwrap_or_default().to_string(),
        (false, false) => format!("{}.{}", parent, child.unwrap_or_default()),
    }
}

fn prefix_key(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{}.{}", prefix, key)
    }
}

fn prefix_path(prefix: &str, path: &str) -> String {
    if path.is_empty() || path.starts_with('/') || prefix.is_empty() {
        return path.to_string();
    }
    format!("{}.{}", prefix, path)
}

fn prefix_validation_value(validation: &mut Value, prefix: &str) {
    if prefix.is_empty() {
        return;
    }
    if let Some(fields) = validation.get_mut("fields").and_then(Value::as_array_mut) {
        for field in fields {
            if let Some(raw) = field.as_str() {
                *field = Value::String(prefix_key(prefix, raw));
            }
        }
    }
    if let Some(condition) = validation.get_mut("condition") {
        prefix_expr_value(condition, prefix);
    }
}

fn prefix_question_value(question: &mut Value, prefix: &str) {
    if prefix.is_empty() {
        return;
    }
    if let Some(id) = question.get_mut("id")
        && let Some(raw) = id.as_str()
    {
        *id = Value::String(prefix_key(prefix, raw));
    }
    if let Some(visible_if) = question.get_mut("visible_if") {
        prefix_expr_value(visible_if, prefix);
    }
    if let Some(computed) = question.get_mut("computed") {
        prefix_expr_value(computed, prefix);
    }
    if let Some(fields) = question
        .get_mut("list")
        .and_then(|list| list.get_mut("fields"))
        .and_then(Value::as_array_mut)
    {
        for field in fields {
            prefix_question_value(field, prefix);
        }
    }
}

fn prefix_expr_value(expr: &mut Value, prefix: &str) {
    if let Some(obj) = expr.as_object_mut() {
        if matches!(
            obj.get("op").and_then(Value::as_str),
            Some("answer") | Some("is_set")
        ) && let Some(path) = obj.get_mut("path")
            && let Some(raw) = path.as_str()
        {
            *path = Value::String(prefix_path(prefix, raw));
        }
        if let Some(inner) = obj.get_mut("expression") {
            prefix_expr_value(inner, prefix);
        }
        if let Some(left) = obj.get_mut("left") {
            prefix_expr_value(left, prefix);
        }
        if let Some(right) = obj.get_mut("right") {
            prefix_expr_value(right, prefix);
        }
        if let Some(items) = obj.get_mut("expressions").and_then(Value::as_array_mut) {
            for item in items {
                prefix_expr_value(item, prefix);
            }
        }
    }
}

fn resolve_context_answers(ctx: &Value) -> Value {
    ctx.get("answers")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()))
}

fn parse_answers(answers_json: &str) -> Value {
    serde_json::from_str(answers_json).unwrap_or_else(|_| Value::Object(Map::new()))
}

fn secrets_host_available(ctx: &Value) -> bool {
    ctx.get("secrets_host_available")
        .and_then(Value::as_bool)
        .or_else(|| {
            ctx.get("config")
                .and_then(Value::as_object)
                .and_then(|config| config.get("secrets_host_available"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
}

fn respond(result: Result<Value, ComponentError>) -> String {
    match result {
        Ok(value) => serde_json::to_string(&value).unwrap_or_else(|error| {
            json!({"error": format!("json encode: {}", error)}).to_string()
        }),
        Err(err) => json!({ "error": err.to_string() }).to_string(),
    }
}

pub fn describe(form_id: &str, config_json: &str) -> String {
    respond(load_form_spec(config_json).and_then(|spec| {
        if spec.id != form_id {
            Err(ComponentError::FormUnavailable(form_id.to_string()))
        } else {
            serde_json::to_value(spec).map_err(ComponentError::JsonEncode)
        }
    }))
}

fn ensure_form(form_id: &str, config_json: &str) -> Result<FormSpec, ComponentError> {
    let spec = load_form_spec(config_json)?;
    if spec.id != form_id {
        Err(ComponentError::FormUnavailable(form_id.to_string()))
    } else {
        Ok(spec)
    }
}

pub fn get_answer_schema(form_id: &str, config_json: &str, ctx_json: &str) -> String {
    let schema = ensure_form(form_id, config_json).map(|spec| {
        let ctx = parse_runtime_context(ctx_json);
        let answers = resolve_context_answers(&ctx);
        let visibility = resolve_visibility(&spec, &answers, VisibilityMode::Visible);
        answers_schema(&spec, &visibility)
    });
    respond(schema)
}

pub fn get_example_answers(form_id: &str, config_json: &str, ctx_json: &str) -> String {
    let result = ensure_form(form_id, config_json).map(|spec| {
        let ctx = parse_runtime_context(ctx_json);
        let answers = resolve_context_answers(&ctx);
        let visibility = resolve_visibility(&spec, &answers, VisibilityMode::Visible);
        example_answers(&spec, &visibility)
    });
    respond(result)
}

pub fn validate_answers(form_id: &str, config_json: &str, answers_json: &str) -> String {
    let validation = ensure_form(form_id, config_json).and_then(|spec| {
        let answers = serde_json::from_str(answers_json).map_err(ComponentError::ConfigParse)?;
        serde_json::to_value(validate(&spec, &answers)).map_err(ComponentError::JsonEncode)
    });
    respond(validation)
}

pub fn next_with_ctx(
    form_id: &str,
    config_json: &str,
    ctx_json: &str,
    answers_json: &str,
) -> String {
    let result = ensure_form(form_id, config_json).map(|spec| {
        let ctx = parse_runtime_context(ctx_json);
        let answers = parse_answers(answers_json);
        let visibility = resolve_visibility(&spec, &answers, VisibilityMode::Visible);
        let progress_ctx = ProgressContext::new(answers.clone(), &ctx);
        let next_q = next_question(&spec, &progress_ctx, &visibility);
        let answered = progress_ctx.answered_count(&spec, &visibility);
        let total = visibility.values().filter(|visible| **visible).count();
        json!({
            "status": if next_q.is_some() { "need_input" } else { "complete" },
            "next_question_id": next_q,
            "progress": {
                "answered": answered,
                "total": total
            }
        })
    });
    respond(result)
}

pub fn next(form_id: &str, config_json: &str, answers_json: &str) -> String {
    next_with_ctx(form_id, config_json, "{}", answers_json)
}

pub fn apply_store(form_id: &str, ctx_json: &str, answers_json: &str) -> String {
    let result = ensure_form(form_id, ctx_json).and_then(|spec| {
        let ctx = parse_runtime_context(ctx_json);
        let answers = parse_answers(answers_json);
        let mut store_ctx = StoreContext::from_value(&ctx);
        store_ctx.answers = answers;
        let host_available = secrets_host_available(&ctx);
        store_ctx.apply_ops(&spec.store, spec.secrets_policy.as_ref(), host_available)?;
        Ok(store_ctx.to_value())
    });
    respond(result)
}

fn render_payload(
    form_id: &str,
    config_json: &str,
    ctx_json: &str,
    answers_json: &str,
) -> Result<RenderPayload, ComponentError> {
    let spec = ensure_form(form_id, config_json)?;
    let ctx = parse_runtime_context(ctx_json);
    let answers = parse_answers(answers_json);
    let mut payload = build_render_payload(&spec, &ctx, &answers);
    let loaded = load_form_spec_value(config_json)?;
    apply_i18n_to_payload(&mut payload, &loaded.spec_value, &ctx);
    Ok(payload)
}

type ResolvedI18nMap = BTreeMap<String, String>;

fn parse_resolved_i18n(ctx: &Value) -> ResolvedI18nMap {
    ctx.get("i18n_resolved")
        .and_then(Value::as_object)
        .map(|value| {
            value
                .iter()
                .filter_map(|(key, val)| val.as_str().map(|text| (key.clone(), text.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn i18n_debug_enabled(ctx: &Value) -> bool {
    ctx.get("debug_i18n")
        .and_then(Value::as_bool)
        .or_else(|| ctx.get("i18n_debug").and_then(Value::as_bool))
        .unwrap_or(false)
}

fn attach_i18n_debug_metadata(card: &mut Value, payload: &RenderPayload, spec_value: &Value) {
    let keys = build_question_i18n_key_map(spec_value);
    let question_metadata = payload
        .questions
        .iter()
        .filter_map(|question| {
            let (title_key, description_key) =
                keys.get(&question.id).cloned().unwrap_or((None, None));
            if title_key.is_none() && description_key.is_none() {
                return None;
            }
            Some(json!({
                "id": question.id,
                "title_key": title_key,
                "description_key": description_key,
            }))
        })
        .collect::<Vec<_>>();
    if question_metadata.is_empty() {
        return;
    }

    if let Some(map) = card.as_object_mut() {
        map.insert(
            "metadata".into(),
            json!({
                "qa": {
                    "i18n_debug": true,
                    "questions": question_metadata
                }
            }),
        );
    }
}

fn build_question_i18n_key_map(
    spec_value: &Value,
) -> BTreeMap<String, (Option<String>, Option<String>)> {
    let mut map = BTreeMap::new();
    for question in spec_value
        .get("questions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        if let Some(id) = question.get("id").and_then(Value::as_str) {
            let title_key = question
                .get("title_i18n")
                .and_then(|value| value.get("key"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let description_key = question
                .get("description_i18n")
                .and_then(|value| value.get("key"))
                .and_then(Value::as_str)
                .map(str::to_string);
            map.insert(id.to_string(), (title_key, description_key));
        }
    }
    map
}

fn resolve_i18n_value(
    resolved: &ResolvedI18nMap,
    key: &str,
    requested_locale: Option<&str>,
    default_locale: Option<&str>,
) -> Option<String> {
    for locale in [requested_locale, default_locale].iter().flatten() {
        if let Some(value) = resolved.get(&format!("{}:{}", locale, key)) {
            return Some(value.clone());
        }
        if let Some(value) = resolved.get(&format!("{}/{}", locale, key)) {
            return Some(value.clone());
        }
    }
    resolved.get(key).cloned()
}

fn apply_i18n_to_payload(payload: &mut RenderPayload, spec_value: &Value, ctx: &Value) {
    let resolved = parse_resolved_i18n(ctx);
    if resolved.is_empty() {
        return;
    }
    let requested_locale = ctx.get("locale").and_then(Value::as_str);
    let default_locale = spec_value
        .get("presentation")
        .and_then(|value| value.get("default_locale"))
        .and_then(Value::as_str);

    let mut by_id = BTreeMap::new();
    for question in spec_value
        .get("questions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        if let Some(id) = question.get("id").and_then(Value::as_str) {
            by_id.insert(id.to_string(), question);
        }
    }

    for question in &mut payload.questions {
        let Some(spec_question) = by_id.get(&question.id) else {
            continue;
        };
        if let Some(key) = spec_question
            .get("title_i18n")
            .and_then(|value| value.get("key"))
            .and_then(Value::as_str)
            && let Some(value) =
                resolve_i18n_value(&resolved, key, requested_locale, default_locale)
        {
            question.title = value;
        }
        if let Some(key) = spec_question
            .get("description_i18n")
            .and_then(|value| value.get("key"))
            .and_then(Value::as_str)
            && let Some(value) =
                resolve_i18n_value(&resolved, key, requested_locale, default_locale)
        {
            question.description = Some(value);
        }
    }
}

fn respond_string(result: Result<String, ComponentError>) -> String {
    match result {
        Ok(value) => value,
        Err(err) => json!({ "error": err.to_string() }).to_string(),
    }
}

pub fn render_text(form_id: &str, config_json: &str, ctx_json: &str, answers_json: &str) -> String {
    respond_string(
        render_payload(form_id, config_json, ctx_json, answers_json)
            .map(|payload| qa_render_text(&payload)),
    )
}

pub fn render_json_ui(
    form_id: &str,
    config_json: &str,
    ctx_json: &str,
    answers_json: &str,
) -> String {
    respond(
        render_payload(form_id, config_json, ctx_json, answers_json)
            .map(|payload| qa_render_json_ui(&payload)),
    )
}

pub fn render_card(form_id: &str, config_json: &str, ctx_json: &str, answers_json: &str) -> String {
    respond(
        render_payload(form_id, config_json, ctx_json, answers_json).map(|payload| {
            let mut card = qa_render_card(&payload);
            let ctx = parse_runtime_context(ctx_json);
            if i18n_debug_enabled(&ctx)
                && let Ok(spec_value) = load_form_spec_value(config_json)
            {
                attach_i18n_debug_metadata(&mut card, &payload, &spec_value.spec_value);
            }
            card
        }),
    )
}

fn submission_progress(payload: &RenderPayload) -> Value {
    json!({
        "answered": payload.progress.answered,
        "total": payload.progress.total,
    })
}

fn build_error_response(
    payload: &RenderPayload,
    answers: Value,
    validation: &qa_spec::ValidationResult,
) -> Result<Value, ComponentError> {
    let validation_value = serde_json::to_value(validation).map_err(ComponentError::JsonEncode)?;
    Ok(json!({
        "status": "error",
        "next_question_id": payload.next_question_id,
        "progress": submission_progress(payload),
        "answers": answers,
        "validation": validation_value,
    }))
}

fn build_success_response(
    payload: &RenderPayload,
    answers: Value,
    store_ctx: &StoreContext,
) -> Value {
    let status = if payload.next_question_id.is_some() {
        "need_input"
    } else {
        "complete"
    };

    json!({
        "status": status,
        "next_question_id": payload.next_question_id,
        "progress": submission_progress(payload),
        "answers": answers,
        "store": store_ctx.to_value(),
    })
}

#[derive(Debug, Clone)]
struct SubmissionPlan {
    validated_patch: Value,
    validation: qa_spec::ValidationResult,
    payload: RenderPayload,
    effects: Vec<StoreOp>,
}

fn build_submission_plan(spec: &FormSpec, ctx: &Value, answers: Value) -> SubmissionPlan {
    let validation = validate(spec, &answers);
    let payload = build_render_payload(spec, ctx, &answers);
    let effects = if validation.valid {
        spec.store.clone()
    } else {
        Vec::new()
    };
    SubmissionPlan {
        validated_patch: answers,
        validation,
        payload,
        effects,
    }
}

pub fn submit_patch(
    form_id: &str,
    config_json: &str,
    ctx_json: &str,
    answers_json: &str,
    question_id: &str,
    value_json: &str,
) -> String {
    // Compatibility wrapper: this endpoint now follows a deterministic
    // plan->execute split internally while preserving existing response shape.
    respond(ensure_form(form_id, config_json).and_then(|spec| {
        let ctx = parse_runtime_context(ctx_json);
        let value: Value = serde_json::from_str(value_json).map_err(ComponentError::ConfigParse)?;
        let mut answers = parse_answers(answers_json)
            .as_object()
            .cloned()
            .unwrap_or_default();
        answers.insert(question_id.to_string(), value);
        let plan = build_submission_plan(&spec, &ctx, Value::Object(answers));

        if !plan.validation.valid {
            return build_error_response(&plan.payload, plan.validated_patch, &plan.validation);
        }

        let mut store_ctx = StoreContext::from_value(&ctx);
        store_ctx.answers = plan.validated_patch.clone();
        let host_available = secrets_host_available(&ctx);
        store_ctx.apply_ops(&plan.effects, spec.secrets_policy.as_ref(), host_available)?;
        let response = build_success_response(&plan.payload, plan.validated_patch, &store_ctx);
        Ok(response)
    }))
}

pub fn submit_all(form_id: &str, config_json: &str, ctx_json: &str, answers_json: &str) -> String {
    // Compatibility wrapper: this endpoint now follows a deterministic
    // plan->execute split internally while preserving existing response shape.
    respond(ensure_form(form_id, config_json).and_then(|spec| {
        let ctx = parse_runtime_context(ctx_json);
        let answers = parse_answers(answers_json);
        let plan = build_submission_plan(&spec, &ctx, answers);

        if !plan.validation.valid {
            return build_error_response(&plan.payload, plan.validated_patch, &plan.validation);
        }

        let mut store_ctx = StoreContext::from_value(&ctx);
        store_ctx.answers = plan.validated_patch.clone();
        let host_available = secrets_host_available(&ctx);
        store_ctx.apply_ops(&plan.effects, spec.secrets_policy.as_ref(), host_available)?;
        let response = build_success_response(&plan.payload, plan.validated_patch, &store_ctx);
        Ok(response)
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalizedMode {
    Setup,
    Update,
    Remove,
}

impl NormalizedMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Update => "update",
            Self::Remove => "remove",
        }
    }

    fn to_qa_mode(self) -> QaMode {
        match self {
            Self::Setup => QaMode::Setup,
            Self::Update => QaMode::Update,
            Self::Remove => QaMode::Remove,
        }
    }
}

pub fn normalize_mode(raw: &str) -> Option<NormalizedMode> {
    match raw {
        "default" | "setup" | "install" => Some(NormalizedMode::Setup),
        "update" | "upgrade" => Some(NormalizedMode::Update),
        "remove" => Some(NormalizedMode::Remove),
        _ => None,
    }
}

fn payload_form_id(payload: &Value) -> String {
    payload
        .get("form_id")
        .and_then(Value::as_str)
        .unwrap_or("example-form")
        .to_string()
}

fn payload_config_json(payload: &Value) -> String {
    if let Some(config_json) = payload.get("config_json").and_then(Value::as_str) {
        return config_json.to_string();
    }
    if let Some(config) = payload.get("config") {
        return config.to_string();
    }
    let mut config = Map::new();
    if let Some(qa_form_asset_path) = payload.get("qa_form_asset_path") {
        config.insert("qa_form_asset_path".to_string(), qa_form_asset_path.clone());
    }
    if let Some(include_registry) = payload.get("include_registry") {
        config.insert("include_registry".to_string(), include_registry.clone());
    }
    if config.is_empty() {
        "{}".to_string()
    } else {
        Value::Object(config).to_string()
    }
}

fn payload_answers(payload: &Value) -> Value {
    if let Some(answers) = payload.get("answers") {
        if let Some(raw) = answers.as_str() {
            return serde_json::from_str(raw).unwrap_or_else(|_| Value::Object(Map::new()));
        }
        return answers.clone();
    }
    Value::Object(Map::new())
}

fn payload_ctx_json(payload: &Value) -> String {
    if let Some(ctx_json) = payload.get("ctx_json").and_then(Value::as_str) {
        return ctx_json.to_string();
    }
    payload
        .get("ctx")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()))
        .to_string()
}

fn mode_title(mode: NormalizedMode) -> (&'static str, &'static str) {
    match mode {
        NormalizedMode::Setup => ("qa.install.title", "qa.install.description"),
        NormalizedMode::Update => ("qa.update.title", "qa.update.description"),
        NormalizedMode::Remove => ("qa.remove.title", "qa.remove.description"),
    }
}

fn question_kind(question: &qa_spec::QuestionSpec) -> QuestionKind {
    match question.kind {
        QuestionType::Boolean => QuestionKind::Bool,
        QuestionType::Integer | QuestionType::Number => QuestionKind::Number,
        QuestionType::Enum => {
            let options = question
                .choices
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|choice| ChoiceOption {
                    value: choice.clone(),
                    label: I18nText::new(
                        format!("qa.field.{}.option.{}", question.id, choice),
                        Some(choice),
                    ),
                })
                .collect();
            QuestionKind::Choice { options }
        }
        QuestionType::List | QuestionType::String => QuestionKind::Text,
    }
}

fn normalize_locale_chain(locale: Option<&str>) -> Vec<String> {
    let Some(raw) = locale else {
        return vec!["en".to_string()];
    };
    let normalized = raw.replace('_', "-");
    let mut chain = vec![normalized.clone()];
    if let Some((base, _)) = normalized.split_once('-') {
        chain.push(base.to_string());
    }
    chain.push("en".to_string());
    chain
}

fn resolve_pack_i18n_text(
    form_asset_path: &str,
    locale: Option<&str>,
    key: &str,
    fallback: Option<&str>,
) -> Option<String> {
    let i18n_dir = infer_i18n_dir_from_form_path(form_asset_path);
    for candidate in normalize_locale_chain(locale) {
        if let Ok(Some(locale_map)) = load_locale_map(&i18n_dir, &candidate)
            && let Some(value) = locale_map.get(key)
        {
            return Some(value.clone());
        }
    }
    fallback.map(str::to_string)
}

fn component_qa_spec(
    mode: NormalizedMode,
    form_id: &str,
    config_json: &str,
    ctx_json: &str,
    answers: &Value,
) -> Result<ComponentQaSpec, ComponentError> {
    let spec = ensure_form(form_id, config_json)?;
    let loaded = load_form_spec_value(config_json)?;
    let ctx = parse_runtime_context(ctx_json);
    let locale = ctx.get("locale").and_then(Value::as_str);
    let visibility = resolve_visibility(&spec, answers, VisibilityMode::Visible);
    let (title_key, description_key) = mode_title(mode);
    let questions = spec
        .questions
        .iter()
        .filter(|question| visibility.get(&question.id).copied().unwrap_or(false))
        .map(|question| {
            let label = question
                .title_i18n
                .as_ref()
                .map(|text| {
                    I18nText::new(
                        text.key.clone(),
                        resolve_pack_i18n_text(
                            &loaded.form_asset_path,
                            locale,
                            &text.key,
                            Some(&question.title),
                        ),
                    )
                })
                .unwrap_or_else(|| {
                    I18nText::new(
                        format!("qa.field.{}.label", question.id),
                        Some(question.title.clone()),
                    )
                });
            let help = match (&question.description_i18n, &question.description) {
                (Some(text), description) => Some(I18nText::new(
                    text.key.clone(),
                    resolve_pack_i18n_text(
                        &loaded.form_asset_path,
                        locale,
                        &text.key,
                        description.as_deref(),
                    ),
                )),
                (None, Some(description)) => Some(I18nText::new(
                    format!("qa.field.{}.help", question.id),
                    Some(description.clone()),
                )),
                (None, None) => None,
            };
            Question {
                id: question.id.clone(),
                label,
                help,
                error: None,
                kind: question_kind(question),
                required: question.required,
                default: None,
            }
        })
        .collect();

    Ok(ComponentQaSpec {
        mode: mode.to_qa_mode(),
        title: I18nText::new(title_key, Some(spec.title)),
        description: spec
            .description
            .map(|description| I18nText::new(description_key, Some(description))),
        questions,
        defaults: BTreeMap::new(),
    })
}

pub fn qa_spec_json(mode: NormalizedMode, payload: &Value) -> Value {
    let form_id = payload_form_id(payload);
    let config_json = payload_config_json(payload);
    let ctx_json = payload_ctx_json(payload);
    let answers = payload_answers(payload);
    match component_qa_spec(mode, &form_id, &config_json, &ctx_json, &answers) {
        Ok(spec) => serde_json::to_value(spec).unwrap_or_else(|_| json!({})),
        Err(err) => json!({
            "mode": mode.as_str(),
            "title": {"key": "qa.error.spec_unavailable", "default": "QA unavailable"},
            "description": {"key": "qa.error.spec_unavailable.description", "default": err.to_string()},
            "questions": [],
            "defaults": {}
        }),
    }
}

pub fn i18n_keys() -> Vec<String> {
    let mut keys = BTreeSet::new();
    for key in crate::i18n::all_keys() {
        keys.insert(key);
    }
    for mode in [
        NormalizedMode::Setup,
        NormalizedMode::Update,
        NormalizedMode::Remove,
    ] {
        let spec = component_qa_spec(mode, "example-form", "", "{}", &json!({}));
        if let Ok(spec) = spec {
            for key in spec.i18n_keys() {
                keys.insert(key);
            }
        }
    }
    keys.into_iter().collect()
}

pub fn apply_answers(mode: NormalizedMode, payload: &Value) -> Value {
    let form_id = payload_form_id(payload);
    let config_json = payload_config_json(payload);
    let answers = payload_answers(payload);
    let current_config = payload
        .get("current_config")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match ensure_form(&form_id, &config_json) {
        Ok(spec) => {
            let validation = validate(&spec, &answers);
            if !validation.valid {
                return json!({
                    "ok": false,
                    "warnings": [],
                    "errors": validation.errors,
                    "meta": {
                        "mode": mode.as_str(),
                        "version": "v1"
                    }
                });
            }

            let mut config = match current_config {
                Value::Object(map) => map,
                _ => Map::new(),
            };
            if let Value::Object(answers) = answers {
                for (key, value) in answers {
                    config.insert(key, value);
                }
            }
            if mode == NormalizedMode::Remove {
                config.insert("enabled".to_string(), Value::Bool(false));
            }

            json!({
                "ok": true,
                "config": config,
                "warnings": [],
                "errors": [],
                "meta": {
                    "mode": mode.as_str(),
                    "version": "v1"
                },
                "audit": {
                    "reasons": ["qa.apply_answers"],
                    "timings_ms": {}
                }
            })
        }
        Err(err) => json!({
            "ok": false,
            "warnings": [],
            "errors": [{"key":"qa.error.spec_unavailable","message": err.to_string()}],
            "meta": {
                "mode": mode.as_str(),
                "version": "v1"
            }
        }),
    }
}
