use std::fmt::Write;

use crate::{t, tf};
use qa_spec::AnswerSet;
use serde_json::Value;

/// Controls which bits of state the wizard prints.
#[derive(Copy, Clone, Eq, PartialEq)]
pub enum Verbosity {
    /// Clean output: question prompts only.
    Clean,
    /// Verbose output: status, visible questions, error details, help text.
    Verbose,
}

impl Verbosity {
    pub fn from_verbose(verbose: bool) -> Self {
        if verbose {
            Verbosity::Verbose
        } else {
            Verbosity::Clean
        }
    }

    pub fn is_verbose(&self) -> bool {
        matches!(self, Verbosity::Verbose)
    }
}

/// Toolbar responsible for printing prompts once the engine yields a question.
pub struct WizardPresenter {
    verbosity: Verbosity,
    header_printed: bool,
    show_answers_json: bool,
}

impl WizardPresenter {
    pub fn new(verbosity: Verbosity, show_answers_json: bool) -> Self {
        Self {
            verbosity,
            header_printed: false,
            show_answers_json,
        }
    }

    pub fn show_header(&mut self, payload: &WizardPayload) {
        if self.header_printed {
            return;
        }
        println!(
            "{}",
            tf("cli.wizard.form", &[("title", payload.form_title.clone())])
        );
        if self.verbosity.is_verbose()
            && let Some(help) = &payload.help
        {
            println!("{}", tf("cli.wizard.help", &[("help", help.clone())]));
        }
        self.header_printed = true;
    }

    pub fn show_status(&self, payload: &WizardPayload) {
        if self.verbosity.is_verbose() {
            println!(
                "{}",
                tf(
                    "cli.wizard.status",
                    &[
                        ("status", payload.status.as_str().to_string()),
                        ("answered", payload.progress.answered.to_string()),
                        ("total", payload.progress.total.to_string()),
                    ]
                )
            );
            self.print_visible_questions(payload);
        } else if payload.status == RenderStatus::NeedInput && payload.visible_count() == 0 {
            println!("{}", t("cli.wizard.no_visible_questions"));
        }
    }

    fn print_visible_questions(&self, payload: &WizardPayload) {
        println!("{}", t("cli.wizard.visible_questions"));
        for question in payload.questions.iter().filter(|question| question.visible) {
            let mut entry = format!(" - {} ({})", question.id, question.title);
            if question.required {
                entry.push_str(" [required]");
            }
            println!("{}", entry);
        }
    }

    pub fn show_prompt(&self, prompt: &PromptContext) {
        let mut line = if prompt.total > 0 {
            format!("{}/{} {}", prompt.index, prompt.total, prompt.title)
        } else {
            format!("{} {}", prompt.index, prompt.title)
        };
        if prompt.required {
            line.push_str(" *");
        }
        if let Some(hint) = &prompt.hint {
            line.push(' ');
            line.push_str(hint);
        }
        println!("{}", line);
        if let Some(description) = &prompt.description {
            println!("{}", description);
        }
        if !prompt.list_fields.is_empty() {
            println!(
                "{}",
                tf(
                    "cli.wizard.list_fields",
                    &[("fields", prompt.list_fields.join(", "))]
                )
            );
        }
        if self.verbosity.is_verbose() && !prompt.choices.is_empty() {
            println!(
                "{}",
                tf(
                    "cli.wizard.choices",
                    &[("choices", prompt.choices.join(", "))]
                )
            );
        }
    }

    pub fn show_parse_error(&self, error: &AnswerParseError) {
        eprintln!(
            "{}",
            tf(
                "cli.wizard.invalid_answer",
                &[("error", error.user_message.clone())]
            )
        );
        if let Some(debug) = &error.debug_message {
            eprintln!(
                "{}",
                tf("cli.wizard.expected", &[("expected", debug.clone())])
            );
        }
    }

    pub fn show_completion(&self, answer_set: &AnswerSet) {
        println!("{}", t("cli.wizard.done"));
        match answer_set.to_cbor() {
            Ok(bytes) => {
                println!(
                    "{}",
                    tf("cli.wizard.answers_cbor", &[("hex", encode_hex(&bytes))])
                );
            }
            Err(err) => {
                eprintln!(
                    "{}",
                    tf(
                        "cli.wizard.cbor_serialize_failed",
                        &[("error", err.to_string())]
                    )
                );
            }
        }
        if self.show_answers_json {
            match answer_set.to_json_pretty() {
                Ok(pretty) => println!("{}", pretty),
                Err(err) => {
                    eprintln!(
                        "{}",
                        tf(
                            "cli.wizard.json_serialize_failed",
                            &[("error", err.to_string())]
                        )
                    );
                }
            }
        }
    }
}

/// Render payload extracted from the component output.
pub struct WizardPayload {
    pub form_title: String,
    pub help: Option<String>,
    pub status: RenderStatus,
    pub progress: RenderProgress,
    pub questions: Vec<WizardQuestion>,
}

impl WizardPayload {
    pub fn from_json(json: &Value) -> Result<Self, String> {
        let form_title = json
            .get("form_title")
            .and_then(Value::as_str)
            .ok_or_else(|| t("cli.wizard.payload_missing_form_title"))?
            .to_string();
        let help = json
            .get("help")
            .and_then(Value::as_str)
            .map(|value| value.to_string());
        let status_str = json
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("need_input");
        let status = RenderStatus::from_label(status_str);
        let progress = json
            .get("progress")
            .and_then(Value::as_object)
            .ok_or_else(|| t("cli.wizard.payload_missing_progress"))?;
        let answered = progress
            .get("answered")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let total = progress.get("total").and_then(Value::as_u64).unwrap_or(0) as usize;
        let questions = json
            .get("questions")
            .and_then(Value::as_array)
            .ok_or_else(|| t("cli.wizard.payload_missing_questions"))?
            .iter()
            .map(WizardQuestion::from_json)
            .collect::<Result<_, _>>()?;
        Ok(Self {
            form_title,
            help,
            status,
            progress: RenderProgress { answered, total },
            questions,
        })
    }

    pub fn visible_count(&self) -> usize {
        self.questions
            .iter()
            .filter(|question| question.visible)
            .count()
    }

    pub fn question(&self, id: &str) -> Option<&WizardQuestion> {
        self.questions.iter().find(|question| question.id == id)
    }
}

/// Progress counters from the render payload.
pub struct RenderProgress {
    pub answered: usize,
    pub total: usize,
}

/// Status returned by the renderer.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum RenderStatus {
    NeedInput,
    Complete,
    Error,
}

impl RenderStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RenderStatus::NeedInput => "need_input",
            RenderStatus::Complete => "complete",
            RenderStatus::Error => "error",
        }
    }

    pub fn from_label(label: &str) -> Self {
        match label {
            "complete" => RenderStatus::Complete,
            "error" => RenderStatus::Error,
            _ => RenderStatus::NeedInput,
        }
    }
}

/// Minimal view of a question used for rendering prompts.
pub struct WizardQuestion {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub kind: QuestionKind,
    pub required: bool,
    pub choices: Vec<String>,
    pub visible: bool,
    pub list_fields: Vec<String>,
}

impl WizardQuestion {
    fn from_json(value: &Value) -> Result<Self, String> {
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| t("cli.wizard.question_missing_id"))?
            .to_string();
        let title = value
            .get("title")
            .and_then(Value::as_str)
            .ok_or_else(|| tf("cli.wizard.question_missing_title", &[("id", id.clone())]))?
            .to_string();
        let description = value
            .get("description")
            .and_then(Value::as_str)
            .map(|value| value.to_string());
        let required = value
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let kind_label = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("string");
        let kind = QuestionKind::from_label(kind_label);
        let choices = value
            .get("choices")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let visible = value
            .get("visible")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let list_fields = value
            .get("list")
            .and_then(Value::as_object)
            .and_then(|list| list.get("fields"))
            .and_then(Value::as_array)
            .map(|fields| {
                fields
                    .iter()
                    .filter_map(|field| field.get("id").and_then(Value::as_str))
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(Self {
            id,
            title,
            description,
            kind,
            required,
            choices,
            visible,
            list_fields,
        })
    }
}

/// Context used to format a single prompt.
pub struct PromptContext {
    pub index: usize,
    pub total: usize,
    pub title: String,
    pub description: Option<String>,
    pub required: bool,
    pub hint: Option<String>,
    pub choices: Vec<String>,
    pub list_fields: Vec<String>,
}

impl PromptContext {
    pub fn new(question: &WizardQuestion, progress: &RenderProgress) -> Self {
        let index = progress.answered + 1;
        let total = progress.total;
        let hint = question.kind.hint(&question.choices);
        Self {
            index: index.max(1),
            total,
            title: question.title.clone(),
            description: question.description.clone(),
            required: question.required,
            hint,
            choices: question.choices.clone(),
            list_fields: question.list_fields.clone(),
        }
    }
}

/// Supported kinds for question prompts.
#[derive(Copy, Clone)]
pub enum QuestionKind {
    String,
    Boolean,
    Integer,
    Number,
    Enum,
    List,
    Unknown,
}

impl QuestionKind {
    fn from_label(label: &str) -> Self {
        match label {
            "string" => QuestionKind::String,
            "boolean" => QuestionKind::Boolean,
            "integer" => QuestionKind::Integer,
            "number" => QuestionKind::Number,
            "enum" => QuestionKind::Enum,
            "list" => QuestionKind::List,
            _ => QuestionKind::Unknown,
        }
    }

    fn hint(&self, choices: &[String]) -> Option<String> {
        match self {
            QuestionKind::Boolean => Some(t("cli.wizard.hint.boolean")),
            QuestionKind::Integer => Some(t("cli.wizard.hint.integer")),
            QuestionKind::Number => Some(t("cli.wizard.hint.number")),
            QuestionKind::Enum if !choices.is_empty() => Some(tf(
                "cli.wizard.hint.enum",
                &[("choices", choices.join("/"))],
            )),
            QuestionKind::List => Some(t("cli.wizard.hint.list")),
            _ => None,
        }
    }
}

/// Error produced when parsing answers from the user.
#[derive(Debug)]
pub struct AnswerParseError {
    pub user_message: String,
    pub debug_message: Option<String>,
}

impl AnswerParseError {
    pub fn new(user_message: impl Into<String>, debug_message: Option<String>) -> Self {
        Self {
            user_message: user_message.into(),
            debug_message,
        }
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{:02x}", byte).expect("writing to string cannot fail");
    }
    encoded
}
