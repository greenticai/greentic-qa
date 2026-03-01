#[cfg(target_arch = "wasm32")]
use std::collections::BTreeMap;

#[cfg(target_arch = "wasm32")]
use greentic_types::cbor::canonical;
#[cfg(target_arch = "wasm32")]
use greentic_types::schemas::common::schema_ir::{AdditionalProperties, SchemaIr};
#[cfg(target_arch = "wasm32")]
use greentic_types::schemas::component::v0_6_0::{
    ComponentDescribe, ComponentInfo, ComponentOperation, ComponentRunInput, ComponentRunOutput,
    I18nText, schema_hash,
};

#[cfg(target_arch = "wasm32")]
mod bindings;
#[cfg(target_arch = "wasm32")]
use bindings::exports::greentic::component::{
    component_descriptor, component_i18n, component_qa, component_runtime, component_schema,
};

pub mod i18n;
pub mod i18n_bundle;
pub mod qa;
pub use qa::{
    apply_store, describe, get_answer_schema, get_example_answers, next, next_with_ctx,
    render_card, render_json_ui, render_text, submit_all, submit_patch, validate_answers,
};

const COMPONENT_NAME: &str = "component-qa";
const COMPONENT_ORG: &str = "ai.greentic";
const COMPONENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(target_arch = "wasm32")]
#[used]
#[unsafe(link_section = ".greentic.wasi")]
static WASI_TARGET_MARKER: [u8; 13] = *b"wasm32-wasip2";

#[cfg(target_arch = "wasm32")]
struct Component;

#[cfg(target_arch = "wasm32")]
impl component_descriptor::Guest for Component {
    fn get_component_info() -> Vec<u8> {
        encode_cbor(&component_info())
    }

    fn describe() -> Vec<u8> {
        encode_cbor(&component_describe())
    }
}

#[cfg(target_arch = "wasm32")]
impl component_schema::Guest for Component {
    fn input_schema() -> Vec<u8> {
        encode_cbor(&input_schema())
    }

    fn output_schema() -> Vec<u8> {
        encode_cbor(&output_schema())
    }

    fn config_schema() -> Vec<u8> {
        encode_cbor(&config_schema())
    }
}

#[cfg(target_arch = "wasm32")]
impl component_runtime::Guest for Component {
    fn run(input: Vec<u8>, state: Vec<u8>) -> component_runtime::RunResult {
        run_component_cbor(input, state)
    }
}

#[cfg(target_arch = "wasm32")]
impl component_qa::Guest for Component {
    fn qa_spec(mode: component_qa::QaMode) -> Vec<u8> {
        let normalized = qa_mode_to_normalized(mode);
        let mut spec = qa::qa_spec_json(normalized, &serde_json::json!({}));
        if matches!(mode, component_qa::QaMode::Default)
            && let Some(spec_obj) = spec.as_object_mut()
        {
            spec_obj.insert(
                "mode".to_string(),
                serde_json::Value::String("default".to_string()),
            );
        }
        encode_cbor(&spec)
    }

    fn apply_answers(
        mode: component_qa::QaMode,
        current_config: Vec<u8>,
        answers: Vec<u8>,
    ) -> Vec<u8> {
        let normalized = qa_mode_to_normalized(mode);
        let payload = serde_json::json!({
            "mode": normalized.as_str(),
            "current_config": parse_payload(&current_config),
            "answers": parse_payload(&answers)
        });
        let result = qa::apply_answers(normalized, &payload);
        let config = result
            .get("config")
            .cloned()
            .or_else(|| payload.get("current_config").cloned())
            .unwrap_or_else(|| serde_json::json!({}));
        encode_cbor(&config)
    }
}

#[cfg(target_arch = "wasm32")]
impl component_i18n::Guest for Component {
    fn i18n_keys() -> Vec<String> {
        qa::i18n_keys()
    }
}

#[cfg(target_arch = "wasm32")]
bindings::export!(Component with_types_in bindings);

pub fn describe_payload() -> String {
    serde_json::json!({
        "component": {
            "name": COMPONENT_NAME,
            "org": COMPONENT_ORG,
            "version": COMPONENT_VERSION,
            "world": "greentic:component/component@0.6.0",
            "schemas": {
                "component": "schemas/component.schema.json",
                "input": "schemas/io/input.schema.json",
                "output": "schemas/io/output.schema.json"
            }
        }
    })
    .to_string()
}

pub fn handle_message(operation: &str, input: &str) -> String {
    format!("{COMPONENT_NAME}::{operation} => {}", input.trim())
}

#[cfg(target_arch = "wasm32")]
fn encode_cbor<T: serde::Serialize>(value: &T) -> Vec<u8> {
    canonical::to_canonical_cbor_allow_floats(value).expect("encode cbor")
}

#[cfg(target_arch = "wasm32")]
fn parse_payload(input: &[u8]) -> serde_json::Value {
    if let Ok(value) = canonical::from_cbor(input) {
        return value;
    }
    serde_json::from_slice(input).unwrap_or_else(|_| serde_json::json!({}))
}

#[cfg(target_arch = "wasm32")]
fn qa_mode_to_normalized(mode: component_qa::QaMode) -> qa::NormalizedMode {
    match mode {
        component_qa::QaMode::Default | component_qa::QaMode::Setup => qa::NormalizedMode::Setup,
        component_qa::QaMode::Update => qa::NormalizedMode::Update,
        component_qa::QaMode::Remove => qa::NormalizedMode::Remove,
    }
}

#[cfg(target_arch = "wasm32")]
fn input_schema() -> SchemaIr {
    SchemaIr::Object {
        properties: BTreeMap::from([(
            "operation".to_string(),
            SchemaIr::String {
                min_len: Some(0),
                max_len: None,
                regex: None,
                format: None,
            },
        )]),
        required: Vec::new(),
        additional: AdditionalProperties::Allow,
    }
}

#[cfg(target_arch = "wasm32")]
fn output_schema() -> SchemaIr {
    SchemaIr::Object {
        properties: BTreeMap::from([(
            "message".to_string(),
            SchemaIr::String {
                min_len: Some(0),
                max_len: None,
                regex: None,
                format: None,
            },
        )]),
        required: Vec::new(),
        additional: AdditionalProperties::Allow,
    }
}

#[cfg(target_arch = "wasm32")]
fn config_schema() -> SchemaIr {
    SchemaIr::Object {
        properties: BTreeMap::from([(
            "qa_form_asset_path".to_string(),
            SchemaIr::String {
                min_len: Some(1),
                max_len: None,
                regex: None,
                format: None,
            },
        )]),
        required: vec!["qa_form_asset_path".to_string()],
        additional: AdditionalProperties::Forbid,
    }
}

#[cfg(target_arch = "wasm32")]
fn component_info() -> ComponentInfo {
    ComponentInfo {
        id: format!("{COMPONENT_ORG}.{COMPONENT_NAME}"),
        version: COMPONENT_VERSION.to_string(),
        role: "tool".to_string(),
        display_name: Some(I18nText::new(
            "component.display_name",
            Some(COMPONENT_NAME.to_string()),
        )),
    }
}

#[cfg(target_arch = "wasm32")]
fn component_describe() -> ComponentDescribe {
    let input = input_schema();
    let output = output_schema();
    let config = config_schema();
    let hash = schema_hash(&input, &output, &config).unwrap_or_default();

    ComponentDescribe {
        info: component_info(),
        provided_capabilities: Vec::new(),
        required_capabilities: Vec::new(),
        metadata: BTreeMap::new(),
        operations: vec![ComponentOperation {
            id: "run".to_string(),
            display_name: Some(I18nText::new("operation.run", Some("Run".to_string()))),
            input: ComponentRunInput {
                schema: input.clone(),
            },
            output: ComponentRunOutput {
                schema: output.clone(),
            },
            defaults: BTreeMap::new(),
            redactions: Vec::new(),
            constraints: BTreeMap::new(),
            schema_hash: hash,
        }],
        config_schema: config,
    }
}

#[cfg(target_arch = "wasm32")]
fn run_component_cbor(input: Vec<u8>, state: Vec<u8>) -> component_runtime::RunResult {
    let value = parse_payload(&input);
    let operation = value
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("handle_message");
    let output = match operation {
        "qa-spec" => {
            let mode = value
                .get("mode")
                .and_then(|v| v.as_str())
                .and_then(qa::normalize_mode)
                .unwrap_or(qa::NormalizedMode::Setup);
            qa::qa_spec_json(mode, &value)
        }
        "apply-answers" => {
            let mode = value
                .get("mode")
                .and_then(|v| v.as_str())
                .and_then(qa::normalize_mode)
                .unwrap_or(qa::NormalizedMode::Setup);
            qa::apply_answers(mode, &value)
        }
        "i18n-keys" => serde_json::Value::Array(
            qa::i18n_keys()
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        ),
        _ => {
            let input_text = value
                .get("input")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| value.to_string());
            serde_json::json!({
                "message": handle_message(operation, &input_text)
            })
        }
    };

    component_runtime::RunResult {
        output: encode_cbor(&output),
        new_state: state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_payload_is_json() {
        let payload = describe_payload();
        let json: serde_json::Value = serde_json::from_str(&payload).expect("valid json");
        assert_eq!(json["component"]["name"], "component-qa");
    }

    #[test]
    fn handle_message_round_trips() {
        let body = handle_message("handle", "demo");
        assert!(body.contains("demo"));
    }
}
