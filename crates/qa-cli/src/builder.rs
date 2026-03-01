use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fmt, fs, io,
    path::{Path, PathBuf},
};

use crate::{t, tf};
use qa_spec::{
    answers_schema::generate as answers_schema,
    examples::generate as example_answers,
    expr::Expr,
    spec::{
        flow::{QAFlowSpec, QuestionStep, StepSpec},
        form::{FormPresentation, FormSpec, ProgressPolicy},
        question::{Constraint, ListSpec, QuestionPolicy, QuestionSpec, QuestionType},
        validation::CrossFieldValidation,
    },
    visibility::{VisibilityMode, resolve_visibility},
};

/// Input shape describing what should be generated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationInput {
    pub dir_name: String,
    pub summary_md: Option<String>,
    pub form: FormInput,
    #[serde(default)]
    pub questions: Vec<QuestionInput>,
    #[serde(default)]
    pub validations: Vec<CrossFieldValidation>,
}

/// Metadata describing the form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormInput {
    pub id: String,
    pub title: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub progress_policy: Option<ProgressPolicyInput>,
}

/// Optional progress directives.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ProgressPolicyInput {
    #[serde(default = "default_skip_answered")]
    pub skip_answered: bool,
    #[serde(default)]
    pub autofill_defaults: bool,
    #[serde(default)]
    pub treat_default_as_answered: bool,
}

fn default_skip_answered() -> bool {
    true
}

impl Default for ProgressPolicyInput {
    fn default() -> Self {
        Self {
            skip_answered: true,
            autofill_defaults: false,
            treat_default_as_answered: false,
        }
    }
}

/// Question metadata collected from CLI interactions or JSON inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionInput {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: CliQuestionType,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_required")]
    pub required: bool,
    #[serde(default)]
    pub default_value: Option<String>,
    #[serde(default)]
    pub choices: Option<Vec<String>>,
    #[serde(default)]
    pub secret: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<ListInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_if: Option<Expr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constraint: Option<Constraint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computed: Option<Expr>,
    #[serde(default)]
    pub computed_overridable: bool,
}

fn default_required() -> bool {
    true
}

/// Supported question types for generation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CliQuestionType {
    #[default]
    String,
    Boolean,
    Integer,
    Number,
    Enum,
    List,
}

impl fmt::Display for CliQuestionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliQuestionType::String => write!(f, "string"),
            CliQuestionType::Boolean => write!(f, "boolean"),
            CliQuestionType::Integer => write!(f, "integer"),
            CliQuestionType::Number => write!(f, "number"),
            CliQuestionType::Enum => write!(f, "enum"),
            CliQuestionType::List => write!(f, "list"),
        }
    }
}

/// Metadata required to describe a repeatable list question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_items: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<QuestionInput>,
}

impl std::str::FromStr for CliQuestionType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_lowercase().as_str() {
            "string" | "str" => Ok(CliQuestionType::String),
            "boolean" | "bool" => Ok(CliQuestionType::Boolean),
            "integer" | "int" => Ok(CliQuestionType::Integer),
            "number" | "float" => Ok(CliQuestionType::Number),
            "enum" | "choice" => Ok(CliQuestionType::Enum),
            "list" => Ok(CliQuestionType::List),
            _ => Err(tf(
                "cli.builder.unknown_question_type",
                &[("value", value.to_string())],
            )),
        }
    }
}

/// Generated bundle returned by the builder.
pub struct GeneratedBundle {
    pub spec: FormSpec,
    pub flow: QAFlowSpec,
    pub schema: Value,
    pub examples: Value,
}

/// Build the full bundle from CLI inputs or JSON answers.
pub fn build_bundle(input: &GenerationInput) -> Result<GeneratedBundle, String> {
    validate_input(input)?;
    let questions = input
        .questions
        .iter()
        .map(to_question_spec)
        .collect::<Vec<_>>();

    let presentation = input.summary_md.as_ref().map(|intro| {
        serde_json::from_value::<FormPresentation>(json!({
            "intro": intro,
            "theme": null,
            "default_locale": null
        }))
        .expect("FormPresentation JSON should deserialize")
    });

    let progress_policy = Some(compute_progress_policy(input.form.progress_policy.as_ref()));

    let form = serde_json::from_value::<FormSpec>(json!({
        "id": input.form.id,
        "title": input.form.title,
        "version": input.form.version,
        "description": input.form.description,
        "presentation": presentation,
        "progress_policy": progress_policy,
        "secrets_policy": null,
        "store": [],
        "validations": input.validations,
        "includes": [],
        "questions": questions
    }))
    .expect("FormSpec JSON should deserialize");

    let answers = Value::Object(Map::new());
    let visibility = resolve_visibility(&form, &answers, VisibilityMode::Visible);
    let schema = answers_schema(&form, &visibility);
    let examples = example_answers(&form, &visibility);
    let flow = build_flow_spec(&form, &input.questions);

    Ok(GeneratedBundle {
        spec: form,
        flow,
        schema,
        examples,
    })
}

fn validate_input(input: &GenerationInput) -> Result<(), String> {
    if input.dir_name.trim().is_empty() {
        return Err(t("cli.builder.dir_name_required"));
    }
    if input.form.id.trim().is_empty() {
        return Err(t("cli.builder.form_id_required"));
    }
    if input.questions.is_empty() {
        return Err(t("cli.builder.at_least_one_question"));
    }

    let mut seen = HashSet::new();
    for question in &input.questions {
        if question.id.trim().is_empty() {
            return Err(t("cli.builder.question_id_empty"));
        }
        if !seen.insert(question.id.clone()) {
            return Err(tf(
                "cli.builder.duplicate_question_id",
                &[("id", question.id.clone())],
            ));
        }
        if matches!(question.kind, CliQuestionType::Enum) {
            let has_choices = question
                .choices
                .as_ref()
                .map(|choices| !choices.is_empty())
                .unwrap_or(false);
            if !has_choices {
                return Err(tf(
                    "cli.builder.enum_question_choices_required",
                    &[("id", question.id.clone())],
                ));
            }
        }

        if matches!(question.kind, CliQuestionType::List) {
            let list = question.list.as_ref().ok_or_else(|| {
                tf(
                    "cli.builder.list_question_metadata_required",
                    &[("id", question.id.clone())],
                )
            })?;
            if list.fields.is_empty() {
                return Err(tf(
                    "cli.builder.list_question_fields_required",
                    &[("id", question.id.clone())],
                ));
            }
            if let (Some(min), Some(max)) = (list.min_items, list.max_items)
                && min > max
            {
                return Err(tf(
                    "cli.builder.list_question_min_gt_max",
                    &[("id", question.id.clone())],
                ));
            }
            let mut seen_fields = HashSet::new();
            for field in &list.fields {
                if field.id.trim().is_empty() {
                    return Err(t("cli.builder.list_field_id_empty"));
                }
                if !seen_fields.insert(field.id.clone()) {
                    return Err(tf(
                        "cli.builder.duplicate_field_id",
                        &[
                            ("field_id", field.id.clone()),
                            ("question_id", question.id.clone()),
                        ],
                    ));
                }
                if matches!(field.kind, CliQuestionType::List) {
                    return Err(t("cli.builder.list_fields_cannot_be_lists"));
                }
            }
        }

        if let Some(constraint) = &question.constraint {
            if let (Some(min), Some(max)) = (constraint.min, constraint.max)
                && min > max
            {
                return Err(tf(
                    "cli.builder.constraint_min_gt_max",
                    &[("min", min.to_string()), ("max", max.to_string())],
                ));
            }
            if let (Some(min_len), Some(max_len)) = (constraint.min_len, constraint.max_len)
                && min_len > max_len
            {
                return Err(tf(
                    "cli.builder.constraint_min_len_gt_max_len",
                    &[
                        ("min_len", min_len.to_string()),
                        ("max_len", max_len.to_string()),
                    ],
                ));
            }
        }
    }

    for validation in &input.validations {
        if validation.message.trim().is_empty() {
            return Err(t("cli.builder.validation_message_required"));
        }
        if validation.fields.is_empty() {
            return Err(t("cli.builder.validation_field_required"));
        }
        for field in &validation.fields {
            if !input.questions.iter().any(|question| question.id == *field) {
                return Err(tf(
                    "cli.builder.validation_unknown_field",
                    &[
                        (
                            "validation",
                            validation
                                .id
                                .as_deref()
                                .unwrap_or(&t("cli.common.unnamed"))
                                .to_string(),
                        ),
                        ("field", field.to_string()),
                    ],
                ));
            }
        }
    }

    Ok(())
}

fn compute_progress_policy(input: Option<&ProgressPolicyInput>) -> ProgressPolicy {
    let policy = input.cloned().unwrap_or_default();
    ProgressPolicy {
        skip_answered: policy.skip_answered,
        autofill_defaults: policy.autofill_defaults,
        treat_default_as_answered: policy.treat_default_as_answered,
    }
}

fn to_question_spec(question: &QuestionInput) -> QuestionSpec {
    let choices = match question.kind {
        CliQuestionType::Enum => question.choices.clone(),
        _ => None,
    };
    let list = question.list.as_ref().map(|list| ListSpec {
        min_items: list.min_items,
        max_items: list.max_items,
        fields: list.fields.iter().map(to_question_spec).collect::<Vec<_>>(),
    });

    serde_json::from_value::<QuestionSpec>(json!({
        "id": question.id,
        "type": question.kind.to_question_type(),
        "title": question.title,
        "title_i18n": null,
        "description": question.description,
        "description_i18n": null,
        "required": question.required,
        "choices": choices,
        "default_value": question.default_value,
        "secret": question.secret,
        "visible_if": question.visible_if,
        "constraint": question.constraint,
        "list": list,
        "policy": QuestionPolicy::default(),
        "computed": question.computed,
        "computed_overridable": question.computed_overridable
    }))
    .expect("QuestionSpec JSON should deserialize")
}

impl CliQuestionType {
    fn to_question_type(self) -> QuestionType {
        match self {
            CliQuestionType::String => QuestionType::String,
            CliQuestionType::Boolean => QuestionType::Boolean,
            CliQuestionType::Integer => QuestionType::Integer,
            CliQuestionType::Number => QuestionType::Number,
            CliQuestionType::Enum => QuestionType::Enum,
            CliQuestionType::List => QuestionType::List,
        }
    }
}

fn build_flow_spec(form: &FormSpec, questions: &[QuestionInput]) -> QAFlowSpec {
    let mut steps = BTreeMap::new();
    let first_step = question_step_id(&questions[0].id);

    for (idx, question) in questions.iter().enumerate() {
        let step_id = question_step_id(&question.id);
        let next_step = if idx + 1 < questions.len() {
            Some(question_step_id(&questions[idx + 1].id))
        } else {
            Some("complete".into())
        };

        steps.insert(
            step_id.clone(),
            StepSpec::Question(QuestionStep {
                question_id: question.id.clone(),
                next: next_step,
            }),
        );
    }

    steps.insert("complete".into(), StepSpec::End);

    QAFlowSpec {
        id: format!("{}-flow", form.id),
        title: format!("{} flow", form.title),
        version: form.version.clone(),
        entry: first_step,
        steps,
        policies: None,
    }
}

fn question_step_id(id: &str) -> String {
    format!("question_{}", sanitize_identifier(id))
}

fn sanitize_identifier(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "step".into()
    } else {
        cleaned
    }
}

/// Serialize the bundle to disk.
pub fn write_bundle(
    bundle: &GeneratedBundle,
    input: &GenerationInput,
    out_root: &Path,
) -> io::Result<PathBuf> {
    let bundle_dir = out_root.join(&input.dir_name);
    let forms_dir = bundle_dir.join("forms");
    let flows_dir = bundle_dir.join("flows");
    let examples_dir = bundle_dir.join("examples");
    let schemas_dir = bundle_dir.join("schemas");

    fs::create_dir_all(&forms_dir)?;
    fs::create_dir_all(&flows_dir)?;
    fs::create_dir_all(&examples_dir)?;
    fs::create_dir_all(&schemas_dir)?;

    let base_name = sanitize_file_name(&bundle.spec.id);

    write_json(
        &forms_dir.join(format!("{}.form.json", base_name)),
        &bundle.spec,
    )?;
    write_json(
        &flows_dir.join(format!("{}.qaflow.json", base_name)),
        &bundle.flow,
    )?;
    write_json(
        &examples_dir.join(format!("{}.answers.example.json", base_name)),
        &bundle.examples,
    )?;
    write_json(
        &schemas_dir.join(format!("{}.answers.schema.json", base_name)),
        &bundle.schema,
    )?;

    let readme_path = bundle_dir.join("README.md");
    fs::write(readme_path, build_readme(bundle, input, &base_name))?;

    Ok(bundle_dir)
}

fn sanitize_file_name(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "bundle".into()
    } else {
        cleaned
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let contents = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    fs::write(path, contents)
}

fn build_readme(bundle: &GeneratedBundle, input: &GenerationInput, base: &str) -> String {
    let summary = input
        .summary_md
        .as_deref()
        .unwrap_or("Generated by `greentic-qa`.");
    let description = input
        .form
        .description
        .as_deref()
        .unwrap_or("No description provided.");

    format!(
        "# {title}\n\nVersion: {version}\n\n{description}\n\n## Summary\n\n{summary}\n\n## Files\n\n- `forms/{base}.form.json`\n- `flows/{base}.qaflow.json`\n- `examples/{base}.answers.example.json`\n- `schemas/{base}.answers.schema.json`\n\nValidate the generated answers with:\n\n```\ngreentic-qa validate --spec forms/{base}.form.json --answers examples/{base}.answers.example.json\n```\n",
        title = bundle.spec.title,
        version = bundle.spec.version,
        description = description,
        summary = summary,
        base = base,
    )
}
