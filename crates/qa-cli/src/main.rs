pub mod builder;

mod cli_i18n;
mod wizard;

use builder::{
    CliQuestionType, FormInput, GeneratedBundle, GenerationInput, ListInput, QuestionInput,
    build_bundle, write_bundle,
};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use cli_i18n::{apply_localized_help, init_from_cli_args};
use greentic_qa_lib::{I18nConfig, ResolvedI18nMap, WizardDriver, WizardFrontend, WizardRunConfig};
use qa_spec::{
    FormSpec, ValidationResult, expr::Expr, spec::question::Constraint,
    spec::validation::CrossFieldValidation, validate,
};
use serde_json::{Number, Value, json};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use wizard::{AnswerParseError, PromptContext, Verbosity, WizardPayload, WizardPresenter};

pub(crate) use cli_i18n::{t, tf};

type CliResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Parser)]
#[command(
    author,
    version,
    about = "Text-based QA wizard CLI",
    long_about = "Provides wizard helpers, spec generation, and validation helpers backed by the QA component"
)]
struct Cli {
    /// Locale used for CLI/runtime i18n lookup (e.g. en-US).
    #[arg(long, global = true, value_name = "LOCALE")]
    locale: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum RenderMode {
    Text,
    Card,
    Json,
}

#[derive(Subcommand)]
enum Command {
    /// Run the existing QA wizard flow in a text shell.
    Wizard {
        /// Path to the FormSpec JSON describing the wizard.
        #[arg(long, value_name = "SPEC")]
        spec: PathBuf,
        /// Optional JSON file containing initial answers.
        #[arg(long, value_name = "ANSWERS")]
        answers: Option<PathBuf>,
        /// Show verbose output (statuses, visible questions, parse expectations).
        #[arg(long, alias = "debug")]
        verbose: bool,
        /// Also emit answer JSON for debugging.
        #[arg(long)]
        answers_json: bool,
        /// Render output mode for the wizard display.
        #[arg(long, value_enum, default_value_t = RenderMode::Text)]
        format: RenderMode,
        /// Path to a JSON object map of resolved i18n keys to strings.
        #[arg(long, value_name = "FILE")]
        i18n_resolved: Option<PathBuf>,
        /// Attach i18n debug metadata to rendered payloads.
        #[arg(long)]
        i18n_debug: bool,
    },
    /// Interactive form generator that creates a bundle of derived artifacts.
    New {
        /// Root directory where the generated bundle will be emitted (defaults to QA_WIZARD_OUTPUT_DIR or current working directory).
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// Overwrite existing bundle if present.
        #[arg(long)]
        force: bool,
        /// Show internal bundle data for debugging.
        #[arg(long)]
        verbose: bool,
    },
    /// Non-interactive generator that consumes JSON answers and emits the bundle.
    Generate {
        /// JSON file describing the form metadata + questions.
        #[arg(long, value_name = "INPUT")]
        input: PathBuf,
        /// Root directory where the generated bundle will be emitted.
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// Overwrite existing bundle if present.
        #[arg(long)]
        force: bool,
        /// Show internal bundle data for debugging.
        #[arg(long)]
        verbose: bool,
    },
    /// Validate answers against a generated FormSpec.
    Validate {
        /// Path to the FormSpec JSON.
        #[arg(long, value_name = "SPEC")]
        spec: PathBuf,
        /// Path to the answers JSON file.
        #[arg(long, value_name = "ANSWERS")]
        answers: PathBuf,
    },
}

struct WizardCliOptions {
    spec_path: PathBuf,
    answers_path: Option<PathBuf>,
    verbose: bool,
    answers_json: bool,
    format: RenderMode,
    locale: Option<String>,
    i18n_resolved: Option<PathBuf>,
    i18n_debug: bool,
}

fn main() -> CliResult<()> {
    let raw_args = env::args().collect::<Vec<_>>();
    init_from_cli_args(&raw_args);
    let cmd = apply_localized_help(Cli::command());
    let matches = cmd.get_matches();
    let cli = Cli::from_arg_matches(&matches)?;
    match cli.command {
        Command::Wizard {
            spec,
            answers,
            verbose,
            answers_json,
            format,
            i18n_resolved,
            i18n_debug,
        } => run_wizard(WizardCliOptions {
            spec_path: spec,
            answers_path: answers,
            verbose,
            answers_json,
            format,
            locale: cli.locale,
            i18n_resolved,
            i18n_debug,
        }),
        Command::New {
            out,
            force,
            verbose,
        } => run_new(out, force, verbose),
        Command::Generate {
            input,
            out,
            force,
            verbose,
        } => run_generate(input, out, force, verbose),
        Command::Validate { spec, answers } => run_validate(spec, answers),
    }
}

fn run_new(out_dir: Option<PathBuf>, force: bool, verbose: bool) -> CliResult<()> {
    println!("{}", t("cli.new.banner"));
    let form_id = prompt_non_empty(&mark_required(&t("cli.prompt.form_id")), None)?;
    let title = prompt_non_empty(&mark_required(&t("cli.prompt.form_title")), None)?;
    let version = prompt_non_empty(&mark_required(&t("cli.prompt.form_version")), Some("0.1.0"))?;
    let description = prompt_optional(&t("cli.prompt.form_description"))?;
    let summary = prompt_optional(&t("cli.prompt.form_summary"))?;
    let dir_name = prompt_non_empty(
        &mark_required(&t("cli.prompt.output_directory_name")),
        Some(&form_id),
    )?;
    let out_root = resolve_output_root(out_dir)?;

    let mut questions = Vec::new();
    loop {
        let question_id = prompt_optional(&t("cli.prompt.question_id"))?;
        let question_id = match question_id.filter(|value| !value.trim().is_empty()) {
            Some(id) => {
                if questions
                    .iter()
                    .any(|question: &QuestionInput| question.id == id)
                {
                    println!(
                        "{}",
                        tf("cli.new.question_id_duplicate", &[("id", id.to_string())])
                    );
                    continue;
                }
                id
            }
            None => break,
        };

        let question_title = prompt_non_empty(
            &mark_required(&t("cli.prompt.question_title")),
            Some(&question_id),
        )?;
        let kind = prompt_question_type()?;
        let required = prompt_bool(&t("cli.prompt.required"), true)?;
        let question_description = prompt_optional(&t("cli.prompt.question_description"))?;
        let choices = if matches!(kind, CliQuestionType::Enum) {
            Some(prompt_enum_choices()?)
        } else {
            None
        };
        let default_prompt = default_prompt_for(kind, choices.as_deref());
        let default_value = loop {
            let candidate = prompt_optional(&default_prompt)?;
            if let Some(value) = &candidate
                && let Err(err) = ensure_default_matches_type(kind, value, choices.as_deref())
            {
                let hint = describe_type_hint(kind, choices.as_deref(), None);
                println!(
                    "{}",
                    tf(
                        "cli.new.invalid_default",
                        &[
                            ("error", err),
                            ("expected", hint.expected),
                            ("example", hint.example),
                        ]
                    )
                );
                continue;
            }
            break candidate;
        };
        let advanced_features = prompt_bool(&t("cli.prompt.advanced_features"), false)?;
        let secret = if advanced_features {
            prompt_bool(&t("cli.prompt.secret_value"), false)?
        } else {
            false
        };
        let list = if matches!(kind, CliQuestionType::List) {
            Some(prompt_list_input()?)
        } else {
            None
        };
        let visible_if = if advanced_features {
            prompt_visibility_condition(&questions)?
        } else {
            None
        };
        let constraint = prompt_constraint(kind)?;
        let (computed, computed_overridable) = if advanced_features {
            prompt_computed_field(kind, &questions)?
        } else {
            (None, false)
        };

        let question = QuestionInput {
            id: question_id,
            kind,
            title: question_title,
            description: question_description,
            required,
            default_value,
            choices,
            secret,
            list,
            visible_if,
            constraint,
            computed,
            computed_overridable,
        };

        if let Err(err) = validate_question_input(&question) {
            let list_fields = question.list.as_ref().map(|list| list.fields.as_slice());
            let hint = describe_type_hint(question.kind, question.choices.as_deref(), list_fields);
            println!(
                "{}",
                tf(
                    "cli.new.invalid_question",
                    &[
                        ("error", err),
                        ("expected", hint.expected),
                        ("example", hint.example),
                    ]
                )
            );
            continue;
        }

        questions.push(question);
    }

    if questions.is_empty() {
        return Err(t("cli.new.at_least_one_question").into());
    }

    let validations = prompt_cross_field_validations(&questions)?;
    let input = GenerationInput {
        dir_name,
        summary_md: summary,
        form: FormInput {
            id: form_id,
            title,
            version,
            description,
            progress_policy: None,
        },
        questions,
        validations,
    };

    let bundle_dir = out_root.join(&input.dir_name);
    ensure_allowed_root(&bundle_dir)?;
    if bundle_dir.exists() {
        if force {
            fs::remove_dir_all(&bundle_dir)?;
        } else {
            return Err(tf(
                "cli.bundle.exists",
                &[("path", bundle_dir.display().to_string())],
            )
            .into());
        }
    }

    let bundle = build_bundle(&input)?;
    let bundle_dir = write_bundle(&bundle, &input, &out_root)?;
    println!(
        "{}",
        tf(
            "cli.bundle.generated",
            &[("path", bundle_dir.display().to_string())]
        )
    );
    if verbose {
        println!("{}", t("cli.bundle.details"));
        dump_bundle_debug(&bundle)?;
    }
    Ok(())
}

fn validate_question_input(question: &QuestionInput) -> Result<(), String> {
    if matches!(question.kind, CliQuestionType::Enum) {
        let has_choices = question
            .choices
            .as_ref()
            .map(|choices| !choices.is_empty())
            .unwrap_or(false);
        if !has_choices {
            return Err(t("cli.new.enum_choices_required"));
        }
    }
    if matches!(question.kind, CliQuestionType::List) {
        let list = question
            .list
            .as_ref()
            .ok_or_else(|| t("cli.new.list_metadata_required"))?;
        if list.fields.is_empty() {
            return Err(t("cli.new.list_fields_required"));
        }
        if let (Some(min), Some(max)) = (list.min_items, list.max_items)
            && min > max
        {
            return Err(t("cli.new.list_min_gt_max"));
        }
    }

    if let Some(default_value) = &question.default_value {
        ensure_default_matches_type(question.kind, default_value, question.choices.as_deref())?;
    }

    Ok(())
}

fn dump_bundle_debug(bundle: &GeneratedBundle) -> CliResult<()> {
    println!("{}", t("cli.bundle.form_spec"));
    println!("{}", serde_json::to_string_pretty(&bundle.spec)?);
    println!("{}", t("cli.bundle.flow_spec"));
    println!("{}", serde_json::to_string_pretty(&bundle.flow)?);
    println!("{}", t("cli.bundle.answer_schema"));
    println!("{}", serde_json::to_string_pretty(&bundle.schema)?);
    println!("{}", t("cli.bundle.example_answers"));
    println!("{}", serde_json::to_string_pretty(&bundle.examples)?);
    Ok(())
}

fn ensure_default_matches_type(
    kind: CliQuestionType,
    default: &str,
    choices: Option<&[String]>,
) -> Result<(), String> {
    match kind {
        CliQuestionType::Boolean => parse_boolean_default(default),
        CliQuestionType::Integer => parse_integer_default(default),
        CliQuestionType::Number => parse_number_default(default),
        CliQuestionType::Enum => parse_enum_default(default, choices),
        CliQuestionType::String => Ok(()),
        CliQuestionType::List => Err(t("cli.new.list_default_not_allowed")),
    }
}

fn parse_boolean_default(raw: &str) -> Result<(), String> {
    match raw.to_lowercase().as_str() {
        "true" | "t" | "yes" | "y" | "1" | "false" | "f" | "no" | "n" | "0" => Ok(()),
        _ => Err(t("cli.new.boolean_default_invalid")),
    }
}

fn parse_integer_default(raw: &str) -> Result<(), String> {
    raw.parse::<i64>()
        .map(|_| ())
        .map_err(|_| t("cli.new.integer_default_invalid"))
}

fn parse_number_default(raw: &str) -> Result<(), String> {
    raw.parse::<f64>()
        .map_err(|_| t("cli.new.number_default_invalid"))
        .and_then(|value| {
            if value.is_finite() {
                Ok(())
            } else {
                Err(t("cli.new.number_default_not_finite"))
            }
        })
}

fn parse_enum_default(raw: &str, choices: Option<&[String]>) -> Result<(), String> {
    let choices = choices.ok_or_else(|| t("cli.new.enum_default_no_choices"))?;
    if choices.iter().any(|choice| choice == raw) {
        Ok(())
    } else {
        Err(tf(
            "cli.new.enum_default_must_match",
            &[("choices", choices.join(", "))],
        ))
    }
}

fn run_generate(
    input_path: PathBuf,
    out_dir: Option<PathBuf>,
    force: bool,
    verbose: bool,
) -> CliResult<()> {
    let contents = fs::read_to_string(&input_path)?;
    let input: GenerationInput = serde_json::from_str(&contents)?;
    let out_root = resolve_output_root(out_dir)?;
    let bundle_dir = out_root.join(&input.dir_name);
    ensure_allowed_root(&bundle_dir)?;
    if bundle_dir.exists() {
        if force {
            fs::remove_dir_all(&bundle_dir)?;
        } else {
            return Err(tf(
                "cli.bundle.exists",
                &[("path", bundle_dir.display().to_string())],
            )
            .into());
        }
    }

    let bundle = build_bundle(&input)?;
    let bundle_dir = write_bundle(&bundle, &input, &out_root)?;
    println!(
        "{}",
        tf(
            "cli.bundle.generated",
            &[("path", bundle_dir.display().to_string())]
        )
    );
    if verbose {
        println!("{}", t("cli.bundle.details"));
        dump_bundle_debug(&bundle)?;
    }
    Ok(())
}

fn run_validate(spec_path: PathBuf, answers_path: PathBuf) -> CliResult<()> {
    let spec_json = fs::read_to_string(&spec_path)?;
    let spec: FormSpec = serde_json::from_str(&spec_json)?;
    let answers_json = fs::read_to_string(answers_path)?;
    let answers: Value = serde_json::from_str(&answers_json)?;

    let result = validate(&spec, &answers);
    println!(
        "{}",
        tf(
            "cli.validate.result",
            &[(
                "result",
                if result.valid {
                    t("cli.validate.valid")
                } else {
                    t("cli.validate.invalid")
                }
            )]
        )
    );
    describe_validation(&result);

    if result.valid {
        Ok(())
    } else {
        Err(t("cli.validate.failed").into())
    }
}

fn describe_validation(result: &ValidationResult) {
    if !result.errors.is_empty() {
        println!("{}", t("cli.validate.errors_header"));
        for error in &result.errors {
            let unknown = t("cli.common.unknown");
            println!(
                "  {} - {}",
                error.path.as_deref().unwrap_or(unknown.as_str()),
                format_validation_error(error)
            );
        }
    }
    if !result.missing_required.is_empty() {
        println!(
            "{}",
            tf(
                "cli.validate.missing_required",
                &[("fields", result.missing_required.join(", "))]
            )
        );
    }
    if !result.unknown_fields.is_empty() {
        println!(
            "{}",
            tf(
                "cli.validate.unknown_fields",
                &[("fields", result.unknown_fields.join(", "))]
            )
        );
    }
}

fn format_validation_error(error: &qa_spec::ValidationError) -> String {
    if error.message.starts_with("qa_spec.") {
        let key = format!("cli.validate.error.{}", error.message);
        let args = error
            .params
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect::<Vec<_>>();
        return tf(&key, &args);
    }
    error.message.clone()
}

fn resolve_output_root(out: Option<PathBuf>) -> CliResult<PathBuf> {
    let candidate = match out {
        Some(path) => path,
        None => env::var_os("QA_WIZARD_OUTPUT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".")),
    };
    if candidate.as_os_str().is_empty() {
        return Err(t("cli.output_dir.empty").into());
    }
    ensure_allowed_root(&candidate)?;
    Ok(candidate)
}

fn ensure_allowed_root(target: &Path) -> CliResult<()> {
    let target = canonicalize_target(target)?;
    let roots = allowed_roots()?;
    if roots.iter().any(|root| target.starts_with(root)) || path_is_writable(&target) {
        Ok(())
    } else {
        Err(format!(
            "path '{}' is outside allowed roots {:?}",
            target.display(),
            roots
        )
        .into())
    }
}

fn allowed_roots() -> CliResult<Vec<PathBuf>> {
    let roots = env::var("QA_WIZARD_ALLOWED_ROOTS")
        .ok()
        .map(|value| {
            value
                .split(':')
                .filter_map(|segment| {
                    let trimmed = segment.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(PathBuf::from(trimmed))
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut canonical_roots = Vec::new();
    for root in roots {
        if let Ok(canonical) = root.canonicalize() {
            canonical_roots.push(canonical);
        } else {
            canonical_roots.push(root);
        }
    }

    if canonical_roots.is_empty() {
        let cwd = env::current_dir()?;
        canonical_roots.push(cwd.canonicalize().unwrap_or(cwd));
    }

    Ok(canonical_roots)
}

fn path_is_writable(target: &Path) -> bool {
    let mut candidate = Some(target);
    while let Some(path) = candidate {
        if path.exists() {
            if let Ok(metadata) = fs::metadata(path) {
                return !metadata.permissions().readonly();
            }
            return false;
        }
        candidate = path.parent();
    }
    false
}

fn canonicalize_target(path: &Path) -> CliResult<PathBuf> {
    if path.exists() {
        return Ok(path.canonicalize()?);
    }

    if let Some(parent) = path.parent()
        && let Ok(parent_canon) = parent.canonicalize()
    {
        if let Some(file_name) = path.file_name() {
            return Ok(parent_canon.join(file_name));
        } else {
            return Ok(parent_canon);
        }
    }

    let cwd = env::current_dir()?;
    Ok(cwd.join(path))
}

fn run_wizard(options: WizardCliOptions) -> CliResult<()> {
    let spec_json = fs::read_to_string(options.spec_path)?;
    let initial_answers_json = if let Some(path) = options.answers_path {
        Some(fs::read_to_string(path)?)
    } else {
        None
    };
    let resolved = if let Some(path) = options.i18n_resolved {
        Some(load_resolved_i18n_map(&path)?)
    } else {
        None
    };

    let frontend = match options.format {
        RenderMode::Text => WizardFrontend::Text,
        RenderMode::Card => WizardFrontend::Card,
        RenderMode::Json => WizardFrontend::JsonUi,
    };

    let config = WizardRunConfig {
        spec_json,
        initial_answers_json,
        frontend,
        i18n: I18nConfig {
            locale: options.locale,
            resolved,
            debug: options.i18n_debug,
        },
        verbose: options.verbose,
    };
    let mut driver = WizardDriver::new(config)?;

    let mut presenter = WizardPresenter::new(
        Verbosity::from_verbose(options.verbose),
        options.answers_json,
    );

    loop {
        let frontend_payload = driver.next_payload_json()?;
        let ui_raw = driver
            .last_ui_json()
            .ok_or_else(|| t("cli.wizard.ui_payload_unavailable"))?
            .to_string();
        let ui: Value = serde_json::from_str(&ui_raw)?;
        print_render_output(options.format, &frontend_payload, Some(&ui_raw))?;

        let payload = WizardPayload::from_json(&ui)
            .map_err(|err| tf("cli.wizard.ui_error", &[("error", err)]))?;
        presenter.show_header(&payload);
        presenter.show_status(&payload);

        if payload.status == wizard::RenderStatus::Complete {
            break;
        }
        let question_id = ui["next_question_id"]
            .as_str()
            .ok_or_else(|| t("cli.wizard.next_question_missing"))?
            .to_string();

        let question = find_question(&ui, &question_id)?;
        let question_info = payload.question(&question_id).ok_or_else(|| {
            tf(
                "cli.wizard.payload_missing_question",
                &[("id", question_id.clone())],
            )
        })?;
        let prompt = PromptContext::new(question_info, &payload.progress);
        let answer = prompt_question(&prompt, &question, &presenter)?;

        let submit = driver.submit_patch_json(&json!({ question_id: answer }).to_string())?;
        let submit_value: Value = serde_json::from_str(&submit.response_json)?;
        let validation = gather_validation_details(&submit_value);

        if submit_value["status"] == "error" {
            if !validation.errors.is_empty() || !validation.unknown_fields.is_empty() {
                print_validation_errors(&validation)?;
                continue;
            }
            if !validation.missing_required.is_empty() {
                print_validation_errors(&validation)?;
            }
        }
    }

    let result = driver.finish()?;
    presenter.show_completion(&result.answer_set);

    Ok(())
}

fn find_question(ui: &Value, question_id: &str) -> CliResult<Value> {
    let question = ui
        .get("questions")
        .and_then(Value::as_array)
        .and_then(|questions| {
            questions
                .iter()
                .find(|question| question["id"].as_str() == Some(question_id))
                .cloned()
        })
        .ok_or_else(|| {
            tf(
                "cli.wizard.question_not_found",
                &[("id", question_id.to_string())],
            )
        })?;
    Ok(question)
}

fn prompt_question(
    prompt: &PromptContext,
    question: &Value,
    presenter: &WizardPresenter,
) -> CliResult<Value> {
    loop {
        presenter.show_prompt(prompt);
        print!("> ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        let trimmed = input.trim();
        if trimmed.eq_ignore_ascii_case("exit") {
            return Err(t("cli.wizard.aborted").into());
        }

        match parse_answer(question, trimmed) {
            Ok(value) => return Ok(value),
            Err(err) => presenter.show_parse_error(&err),
        }
    }
}

fn parse_answer(question: &Value, raw: &str) -> Result<Value, AnswerParseError> {
    let prompt_value = if raw.is_empty() {
        question
            .get("default")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    } else {
        raw.trim().to_string()
    };

    if prompt_value.is_empty() {
        if !question
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            return Ok(Value::Null);
        }
        return Err(AnswerParseError::new(t("cli.wizard.required_answer"), None));
    }

    match question
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("string")
    {
        "boolean" => parse_boolean(&prompt_value),
        "integer" => parse_integer(&prompt_value),
        "number" => parse_number(&prompt_value),
        "enum" => parse_enum(question, &prompt_value),
        "list" => parse_list(question, &prompt_value),
        _ => Ok(Value::String(prompt_value)),
    }
}

fn parse_boolean(raw: &str) -> Result<Value, AnswerParseError> {
    match raw.to_lowercase().as_str() {
        "true" | "t" | "yes" | "y" | "1" => Ok(Value::Bool(true)),
        "false" | "f" | "no" | "n" | "0" => Ok(Value::Bool(false)),
        _ => Err(AnswerParseError::new(
            t("cli.parse.boolean_prompt"),
            Some("expected boolean (y/n/true/false)".to_string()),
        )),
    }
}

fn parse_integer(raw: &str) -> Result<Value, AnswerParseError> {
    raw.parse::<i64>()
        .map(Number::from)
        .map(Value::Number)
        .map_err(|_| {
            AnswerParseError::new(
                t("cli.parse.integer_prompt"),
                Some("expected integer".to_string()),
            )
        })
}

fn parse_number(raw: &str) -> Result<Value, AnswerParseError> {
    raw.parse::<f64>()
        .map_err(|_| {
            AnswerParseError::new(
                t("cli.parse.number_prompt"),
                Some("expected number".to_string()),
            )
        })
        .and_then(|value| {
            serde_json::Number::from_f64(value)
                .map(Value::Number)
                .ok_or_else(|| {
                    AnswerParseError::new(
                        t("cli.parse.number_finite"),
                        Some("number must be finite".to_string()),
                    )
                })
        })
}

fn parse_enum(question: &Value, raw: &str) -> Result<Value, AnswerParseError> {
    let choices = question
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| AnswerParseError::new(t("cli.parse.choices_missing"), None))?;

    let allowed = choices
        .iter()
        .filter_map(Value::as_str)
        .map(String::from)
        .collect::<Vec<_>>();

    if let Some(choice) = allowed
        .iter()
        .find(|choice| choice.eq_ignore_ascii_case(raw))
    {
        Ok(Value::String(choice.to_string()))
    } else {
        Err(AnswerParseError::new(
            tf(
                "cli.parse.choose_one_of",
                &[("choices", allowed.join(", "))],
            ),
            Some(tf(
                "cli.parse.allowed_values",
                &[("choices", allowed.join(", "))],
            )),
        ))
    }
}

fn parse_list(question: &Value, raw: &str) -> Result<Value, AnswerParseError> {
    match serde_json::from_str::<Value>(raw) {
        Ok(value) if value.is_array() => Ok(value),
        Ok(_) => Err(AnswerParseError::new(
            t("cli.parse.list_array"),
            Some(tf(
                "cli.parse.list_expected_fields",
                &[("fields", describe_list_fields(question))],
            )),
        )),
        Err(err) => Err(AnswerParseError::new(
            t("cli.parse.list_invalid"),
            Some(err.to_string()),
        )),
    }
}

fn describe_list_fields(question: &Value) -> String {
    question
        .get("list")
        .and_then(Value::as_object)
        .and_then(|list| list.get("fields"))
        .and_then(Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .filter_map(|field| field.get("id").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|description| !description.is_empty())
        .unwrap_or_else(|| t("cli.common.unknown"))
}

fn prompt_line(prompt: &str, default: Option<&str>) -> CliResult<String> {
    if let Some(default_value) = default {
        print!("{} [{}]: ", prompt, default_value);
    } else {
        print!("{}: ", prompt);
    }
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        if let Some(default_value) = default {
            Ok(default_value.to_string())
        } else {
            Ok(String::new())
        }
    } else {
        Ok(trimmed.to_string())
    }
}

fn prompt_optional(prompt: &str) -> CliResult<Option<String>> {
    let value = prompt_line(prompt, None)?;
    if value.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

fn prompt_non_empty(prompt: &str, default: Option<&str>) -> CliResult<String> {
    loop {
        let value = prompt_line(prompt, default)?;
        if !value.trim().is_empty() {
            return Ok(value);
        }
        println!("{}", t("cli.prompt.value_empty"));
    }
}

fn mark_required(prompt: &str) -> String {
    tf(
        "cli.prompt.required_label",
        &[("label", prompt.trim().to_string())],
    )
}

fn describe_list_size(min_items: Option<usize>, max_items: Option<usize>) -> String {
    match (min_items, max_items) {
        (Some(min), Some(max)) => tf(
            "cli.prompt.list_size_range",
            &[("min", min.to_string()), ("max", max.to_string())],
        ),
        (Some(min), None) => tf("cli.prompt.list_size_min", &[("min", min.to_string())]),
        (None, Some(max)) => tf("cli.prompt.list_size_max", &[("max", max.to_string())]),
        (None, None) => t("cli.prompt.list_size_unrestricted"),
    }
}

fn summarize_list_fields(fields: &[QuestionInput]) -> String {
    fields
        .iter()
        .map(|field| format!("{} ({})", field.id, field.kind))
        .collect::<Vec<_>>()
        .join(", ")
}

struct TypeHint {
    expected: String,
    example: String,
}

fn describe_type_hint(
    kind: CliQuestionType,
    choices: Option<&[String]>,
    list_fields: Option<&[QuestionInput]>,
) -> TypeHint {
    match kind {
        CliQuestionType::String => TypeHint {
            expected: t("cli.type_hint.string.expected"),
            example: t("cli.type_hint.string.example"),
        },
        CliQuestionType::Boolean => TypeHint {
            expected: t("cli.type_hint.boolean.expected"),
            example: t("cli.type_hint.boolean.example"),
        },
        CliQuestionType::Integer => TypeHint {
            expected: t("cli.type_hint.integer.expected"),
            example: t("cli.type_hint.integer.example"),
        },
        CliQuestionType::Number => TypeHint {
            expected: t("cli.type_hint.number.expected"),
            example: t("cli.type_hint.number.example"),
        },
        CliQuestionType::Enum => {
            let mut expected = t("cli.type_hint.enum.expected");
            if let Some(values) = choices
                && !values.is_empty()
            {
                expected = tf(
                    "cli.type_hint.enum.one_of",
                    &[("choices", values.join(", "))],
                );
            }
            let example = choices
                .and_then(|values| values.first())
                .cloned()
                .unwrap_or_else(|| t("cli.type_hint.enum.example"));
            TypeHint { expected, example }
        }
        CliQuestionType::List => {
            let fields_desc = list_fields
                .map(summarize_list_fields)
                .unwrap_or_else(|| t("cli.type_hint.list.fields"));
            TypeHint {
                expected: tf("cli.type_hint.list.expected", &[("fields", fields_desc)]),
                example: t("cli.type_hint.list.example"),
            }
        }
    }
}

fn prompt_visibility_condition(questions: &[QuestionInput]) -> CliResult<Option<Expr>> {
    if questions.is_empty() || !prompt_bool(&t("cli.prompt.add_visibility_condition"), false)? {
        return Ok(None);
    }
    println!(
        "{}",
        tf(
            "cli.prompt.existing_questions",
            &[("ids", existing_question_ids(questions))]
        )
    );
    let expr = prompt_boolean_expression(questions, 0)?;
    Ok(Some(expr))
}

fn prompt_boolean_expression(questions: &[QuestionInput], depth: usize) -> CliResult<Expr> {
    const MAX_DEPTH: usize = 4;
    let mut prompt = t("cli.prompt.expr_type_prefix");
    if depth < MAX_DEPTH {
        prompt.push_str("/and/or/not");
    }
    prompt.push(')');
    let choice = prompt_line(&prompt, Some(&t("cli.prompt.expr_type_default")))?;
    match choice.trim().to_lowercase().as_str() {
        "is_set" => prompt_is_set_expression(questions),
        "and" if depth < MAX_DEPTH => {
            let left = prompt_boolean_expression(questions, depth + 1)?;
            let right = prompt_boolean_expression(questions, depth + 1)?;
            Ok(Expr::And {
                expressions: vec![left, right],
            })
        }
        "or" if depth < MAX_DEPTH => {
            let left = prompt_boolean_expression(questions, depth + 1)?;
            let right = prompt_boolean_expression(questions, depth + 1)?;
            Ok(Expr::Or {
                expressions: vec![left, right],
            })
        }
        "not" if depth < MAX_DEPTH => {
            let inner = prompt_boolean_expression(questions, depth + 1)?;
            Ok(Expr::Not {
                expression: Box::new(inner),
            })
        }
        _ => {
            println!("{}", t("cli.prompt.building_comparison"));
            prompt_comparison_expression(questions)
        }
    }
}

fn prompt_comparison_expression(questions: &[QuestionInput]) -> CliResult<Expr> {
    println!(
        "{}",
        tf(
            "cli.prompt.existing_questions",
            &[("ids", existing_question_ids(questions))]
        )
    );
    let operator = prompt_line(&t("cli.prompt.operator"), Some("eq"))?;
    let normalized = operator.trim().to_lowercase();
    let left_id = prompt_non_empty(&t("cli.prompt.question_id_compare"), None)?;
    let left_expr = Expr::Answer { path: left_id };
    let operand = prompt_line(
        &t("cli.prompt.right_operand_type"),
        Some(&t("cli.prompt.right_operand_default")),
    )?;
    let right_expr = match operand.trim().to_lowercase().as_str() {
        "question" | "answer" => {
            let right_id = prompt_non_empty(&t("cli.prompt.right_operand_question_id"), None)?;
            Expr::Answer { path: right_id }
        }
        _ => {
            let value = prompt_non_empty(&t("cli.prompt.value_compare_against"), None)?;
            Expr::Literal {
                value: parse_expression_literal(&value),
            }
        }
    };
    Ok(build_binary_expression(&normalized, left_expr, right_expr))
}

fn prompt_is_set_expression(questions: &[QuestionInput]) -> CliResult<Expr> {
    println!(
        "{}",
        tf(
            "cli.prompt.existing_questions",
            &[("ids", existing_question_ids(questions))]
        )
    );
    let target = prompt_non_empty(&t("cli.prompt.question_id_presence"), None)?;
    Ok(Expr::IsSet { path: target })
}

fn prompt_cross_field_validations(
    questions: &[QuestionInput],
) -> CliResult<Vec<CrossFieldValidation>> {
    let mut validations = Vec::new();
    while prompt_bool(&t("cli.prompt.add_cross_field_validation"), false)? {
        let id = prompt_optional(&t("cli.prompt.validation_id"))?;
        let message = prompt_non_empty(&t("cli.prompt.validation_message"), None)?;
        let fields = prompt_validation_fields(questions)?;
        let condition = prompt_boolean_expression(questions, 0)?;
        validations.push(CrossFieldValidation {
            id,
            message,
            fields,
            condition,
            code: None,
        });
    }
    Ok(validations)
}

fn prompt_validation_fields(questions: &[QuestionInput]) -> CliResult<Vec<String>> {
    loop {
        println!(
            "{}",
            tf(
                "cli.prompt.available_questions",
                &[("ids", existing_question_ids(questions))]
            )
        );
        let raw = prompt_line(&t("cli.prompt.fields_to_validate"), None)?;
        let mut fields = raw
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(String::from)
            .collect::<Vec<_>>();
        fields.dedup();
        if fields.is_empty() {
            println!("{}", t("cli.prompt.at_least_one_field"));
            continue;
        }
        let unknown = fields
            .iter()
            .filter(|field| !question_exists(questions, field))
            .cloned()
            .collect::<Vec<_>>();
        if !unknown.is_empty() {
            println!(
                "{}",
                tf(
                    "cli.prompt.unknown_fields",
                    &[("fields", unknown.join(", "))]
                )
            );
            continue;
        }
        return Ok(fields);
    }
}

fn question_exists(questions: &[QuestionInput], candidate: &str) -> bool {
    questions.iter().any(|question| question.id == candidate)
}

fn prompt_computed_field(
    kind: CliQuestionType,
    existing: &[QuestionInput],
) -> CliResult<(Option<Expr>, bool)> {
    if matches!(kind, CliQuestionType::List)
        || !prompt_bool(&t("cli.prompt.compute_question_value"), false)?
    {
        return Ok((None, false));
    }
    println!(
        "{}",
        tf(
            "cli.prompt.existing_questions",
            &[("ids", existing_question_ids(existing))]
        )
    );
    loop {
        let source = prompt_line(
            &t("cli.prompt.computed_source"),
            Some(&t("cli.prompt.computed_source_default")),
        )?;
        let normalized = source.trim().to_lowercase();
        match normalized.as_str() {
            "answer" => {
                let question = prompt_non_empty(&t("cli.prompt.source_question_id"), None)?;
                let overrides = prompt_bool(&t("cli.prompt.allow_override_computed"), false)?;
                return Ok((Some(Expr::Answer { path: question }), overrides));
            }
            "literal" => {
                let literal = prompt_non_empty(&t("cli.prompt.literal_value"), None)?;
                let overrides = prompt_bool(&t("cli.prompt.allow_override_computed"), false)?;
                return Ok((
                    Some(Expr::Literal {
                        value: parse_expression_literal(&literal),
                    }),
                    overrides,
                ));
            }
            _ => {
                println!(
                    "{}",
                    tf(
                        "cli.prompt.unknown_source",
                        &[("source", normalized.clone())]
                    )
                );
            }
        }
    }
}

fn prompt_constraint(kind: CliQuestionType) -> CliResult<Option<Constraint>> {
    let mut constraint = Constraint {
        pattern: None,
        min: None,
        max: None,
        min_len: None,
        max_len: None,
    };
    let mut changed = false;
    if matches!(kind, CliQuestionType::Integer | CliQuestionType::Number) {
        if let Some(min) = prompt_optional_f64(&t("cli.prompt.min_numeric_value"))? {
            constraint.min = Some(min);
            changed = true;
        }
        if let Some(max) = prompt_optional_f64(&t("cli.prompt.max_numeric_value"))? {
            constraint.max = Some(max);
            changed = true;
        }
    }
    if matches!(kind, CliQuestionType::String | CliQuestionType::Enum) {
        if let Some(min_len) = prompt_optional_usize(&t("cli.prompt.min_length"))? {
            constraint.min_len = Some(min_len);
            changed = true;
        }
        if let Some(max_len) = prompt_optional_usize(&t("cli.prompt.max_length"))? {
            constraint.max_len = Some(max_len);
            changed = true;
        }
        if let Some(pattern) = prompt_optional(&t("cli.prompt.regex_pattern"))?
            && !pattern.trim().is_empty()
        {
            constraint.pattern = Some(pattern);
            changed = true;
        }
    }
    if changed {
        Ok(Some(constraint))
    } else {
        Ok(None)
    }
}

fn prompt_optional_f64(prompt: &str) -> CliResult<Option<f64>> {
    loop {
        let raw = prompt_line(prompt, None)?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        match trimmed.parse::<f64>() {
            Ok(value) => return Ok(Some(value)),
            Err(_) => {
                println!("{}", t("cli.prompt.enter_number_or_blank"));
            }
        }
    }
}

fn parse_expression_literal(raw: &str) -> Value {
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("true") {
        return Value::Bool(true);
    }
    if trimmed.eq_ignore_ascii_case("false") {
        return Value::Bool(false);
    }
    if let Ok(int_val) = trimmed.parse::<i64>() {
        return Value::Number(Number::from(int_val));
    }
    if let Ok(float_val) = trimmed.parse::<f64>()
        && let Some(number) = Number::from_f64(float_val)
    {
        return Value::Number(number);
    }
    if let Ok(json_val) = serde_json::from_str::<Value>(trimmed) {
        return json_val;
    }
    Value::String(trimmed.to_string())
}

fn existing_question_ids(questions: &[QuestionInput]) -> String {
    if questions.is_empty() {
        t("cli.common.none")
    } else {
        questions
            .iter()
            .map(|question| question.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn build_binary_expression(operator: &str, left: Expr, right: Expr) -> Expr {
    match operator {
        "eq" => Expr::Eq {
            left: Box::new(left),
            right: Box::new(right),
        },
        "ne" => Expr::Ne {
            left: Box::new(left),
            right: Box::new(right),
        },
        "lt" => Expr::Lt {
            left: Box::new(left),
            right: Box::new(right),
        },
        "lte" => Expr::Lte {
            left: Box::new(left),
            right: Box::new(right),
        },
        "gt" => Expr::Gt {
            left: Box::new(left),
            right: Box::new(right),
        },
        "gte" => Expr::Gte {
            left: Box::new(left),
            right: Box::new(right),
        },
        _ => Expr::Eq {
            left: Box::new(left),
            right: Box::new(right),
        },
    }
}

fn render_list_example(fields: &[QuestionInput]) -> String {
    let entries = fields
        .iter()
        .map(|field| {
            let hint = describe_type_hint(field.kind, field.choices.as_deref(), None);
            format!("\"{}\": {}", field.id, hint.example)
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{{ {} }}]", entries)
}

fn prompt_bool(prompt: &str, default: bool) -> CliResult<bool> {
    let prompt_text = tf(
        "cli.prompt.yes_no",
        &[("prompt", prompt.trim().to_string())],
    );
    let default_hint = if default { "Y" } else { "N" };
    loop {
        let line = prompt_line(&prompt_text, Some(default_hint))?;
        match line.trim().to_lowercase().as_str() {
            "" => return Ok(default),
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            other => {
                println!(
                    "{}",
                    tf(
                        "cli.prompt.invalid_yes_no",
                        &[("answer", other.to_string())]
                    )
                );
            }
        }
    }
}

fn prompt_question_type() -> CliResult<CliQuestionType> {
    loop {
        let value = prompt_line(&t("cli.prompt.question_type"), Some("string"))?;
        match CliQuestionType::from_str(&value) {
            Ok(kind) => return Ok(kind),
            Err(err) => println!("{}", err),
        }
    }
}

fn prompt_enum_choices() -> CliResult<Vec<String>> {
    loop {
        let raw = prompt_line(&t("cli.prompt.enum_choices"), None)?;
        let normalized = raw
            .split(',')
            .map(str::trim)
            .filter(|choice| !choice.is_empty())
            .map(|choice| choice.to_string())
            .collect::<Vec<_>>();
        if normalized.is_empty() {
            println!("{}", t("cli.prompt.enum_choices_required"));
            continue;
        }
        return Ok(normalized);
    }
}

fn prompt_optional_usize(prompt: &str) -> CliResult<Option<usize>> {
    loop {
        let raw = prompt_line(prompt, None)?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        match trimmed.parse::<usize>() {
            Ok(value) => return Ok(Some(value)),
            Err(_) => {
                println!("{}", t("cli.prompt.enter_whole_number_or_blank"));
            }
        }
    }
}

fn prompt_list_input() -> CliResult<ListInput> {
    loop {
        let min_items = prompt_optional_usize(&t("cli.prompt.min_items"))?;
        let max_items = prompt_optional_usize(&t("cli.prompt.max_items"))?;
        if let (Some(min), Some(max)) = (min_items, max_items)
            && min > max
        {
            println!("{}", t("cli.prompt.min_items_gt_max_items"));
            continue;
        }

        println!(
            "{}",
            tf(
                "cli.prompt.list_size",
                &[("size", describe_list_size(min_items, max_items))]
            )
        );

        let fields = prompt_list_fields()?;
        if fields.is_empty() {
            println!("{}", t("cli.prompt.list_requires_field"));
            continue;
        }

        println!(
            "{}",
            tf(
                "cli.prompt.defined_list_fields",
                &[
                    ("count", fields.len().to_string()),
                    ("fields", summarize_list_fields(&fields)),
                ]
            )
        );
        println!(
            "{}",
            tf(
                "cli.prompt.example_list_entry",
                &[("entry", render_list_example(&fields))]
            )
        );

        return Ok(ListInput {
            min_items,
            max_items,
            fields,
        });
    }
}

fn prompt_list_fields() -> CliResult<Vec<QuestionInput>> {
    let mut fields: Vec<QuestionInput> = Vec::new();
    loop {
        let field_id = prompt_optional(&t("cli.prompt.field_id"))?;
        let field_id = match field_id.filter(|value| !value.trim().is_empty()) {
            Some(id) => {
                if fields.iter().any(|field| field.id == id) {
                    println!(
                        "{}",
                        tf("cli.prompt.field_id_duplicate", &[("id", id.to_string())])
                    );
                    continue;
                }
                id
            }
            None => break,
        };

        let field_title = prompt_non_empty(&mark_required("Field title"), Some(&field_id))?;
        let field_kind = loop {
            let kind = prompt_question_type()?;
            if matches!(kind, CliQuestionType::List) {
                println!("{}", t("cli.prompt.nested_list_not_allowed"));
                continue;
            }
            break kind;
        };
        let required = prompt_bool(&t("cli.prompt.field_required"), true)?;
        let field_description = prompt_optional(&t("cli.prompt.field_description"))?;
        let field_choices = if matches!(field_kind, CliQuestionType::Enum) {
            Some(prompt_enum_choices()?)
        } else {
            None
        };
        let default_prompt = default_prompt_for(field_kind, field_choices.as_deref());
        let field_default = loop {
            let candidate = prompt_optional(&default_prompt)?;
            if let Some(value) = &candidate
                && let Err(err) =
                    ensure_default_matches_type(field_kind, value, field_choices.as_deref())
            {
                println!(
                    "{}",
                    tf("cli.prompt.invalid_default_retry", &[("error", err)])
                );
                continue;
            }
            break candidate;
        };
        let field_secret = prompt_bool(&t("cli.prompt.field_secret"), false)?;
        let field_hint = describe_type_hint(field_kind, field_choices.as_deref(), None);
        let field_input = QuestionInput {
            id: field_id.clone(),
            kind: field_kind,
            title: field_title,
            description: field_description,
            required,
            default_value: field_default,
            choices: field_choices,
            secret: field_secret,
            list: None,
            visible_if: None,
            constraint: None,
            computed: None,
            computed_overridable: false,
        };
        if let Err(err) = validate_question_input(&field_input) {
            println!(
                "{}",
                tf("cli.prompt.invalid_field_retry", &[("error", err)])
            );
            continue;
        }
        fields.push(field_input);
        println!(
            "{}",
            tf(
                "cli.prompt.added_list_field",
                &[
                    ("id", field_id.clone()),
                    ("kind", field_kind.to_string()),
                    ("count", fields.len().to_string()),
                ]
            )
        );
        println!(
            "{}",
            tf(
                "cli.prompt.field_hint",
                &[
                    ("expected", field_hint.expected),
                    ("example", field_hint.example),
                ]
            )
        );
    }

    Ok(fields)
}

fn default_prompt_for(kind: CliQuestionType, choices: Option<&[String]>) -> String {
    match kind {
        CliQuestionType::Boolean => t("cli.prompt.default_value_boolean"),
        CliQuestionType::Integer => t("cli.prompt.default_value_integer"),
        CliQuestionType::Number => t("cli.prompt.default_value_number"),
        CliQuestionType::Enum => match choices {
            Some(choices) if !choices.is_empty() => tf(
                "cli.prompt.default_value_enum_one_of",
                &[("choices", choices.join("/"))],
            ),
            _ => t("cli.prompt.default_value_enum"),
        },
        _ => t("cli.prompt.default_value"),
    }
}

struct ValidationDetails {
    errors: Vec<(String, String)>,
    missing_required: Vec<String>,
    unknown_fields: Vec<String>,
}

fn gather_validation_details(response: &Value) -> ValidationDetails {
    let validation = response.get("validation");

    let errors = validation
        .and_then(|value| value.get("errors"))
        .and_then(Value::as_array)
        .map(|array| {
            array
                .iter()
                .map(|error| {
                    let unknown = t("cli.common.unknown");
                    let failed = t("cli.validate.failed");
                    let path = error
                        .get("path")
                        .and_then(Value::as_str)
                        .unwrap_or(unknown.as_str())
                        .to_string();
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or(failed.as_str())
                        .to_string();
                    (path, message)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let missing_required = validation
        .and_then(|value| value.get("missing_required"))
        .and_then(Value::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let unknown_fields = validation
        .and_then(|value| value.get("unknown_fields"))
        .and_then(Value::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    ValidationDetails {
        errors,
        missing_required,
        unknown_fields,
    }
}

fn print_validation_errors(details: &ValidationDetails) -> CliResult<()> {
    if !details.errors.is_empty() {
        eprintln!("{}", t("cli.validate.errors_header"));
        for (path, message) in &details.errors {
            eprintln!("  {}: {}", path, message);
        }
    }

    if !details.missing_required.is_empty() {
        eprintln!(
            "{}",
            tf(
                "cli.validate.missing_required",
                &[("fields", details.missing_required.join(", "))]
            )
        );
    }

    if !details.unknown_fields.is_empty() {
        eprintln!(
            "{}",
            tf(
                "cli.validate.unknown_fields",
                &[("fields", details.unknown_fields.join(", "))]
            )
        );
    }

    Ok(())
}

fn print_render_output(
    mode: RenderMode,
    frontend_payload_json: &str,
    ui: Option<&str>,
) -> CliResult<()> {
    match mode {
        RenderMode::Text => Ok(()),
        RenderMode::Card => {
            println!(
                "{}",
                tf(
                    "cli.output.adaptive_card",
                    &[("payload", frontend_payload_json.to_string())]
                )
            );
            Ok(())
        }
        RenderMode::Json => {
            if let Some(ui) = ui {
                println!(
                    "{}",
                    tf("cli.output.json_ui", &[("payload", ui.to_string())])
                );
            } else {
                println!(
                    "{}",
                    tf(
                        "cli.output.json_ui",
                        &[("payload", frontend_payload_json.to_string())]
                    )
                );
            }
            Ok(())
        }
    }
}

fn load_resolved_i18n_map(path: &Path) -> CliResult<ResolvedI18nMap> {
    let raw = fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&raw)?;
    let object = value
        .as_object()
        .ok_or_else(|| t("cli.i18n_resolved.flat_map_required"))?;

    let mut map = ResolvedI18nMap::new();
    for (key, value) in object {
        let text = value
            .as_str()
            .ok_or_else(|| t("cli.i18n_resolved.flat_map_required"))?;
        map.insert(key.clone(), text.to_string());
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_cmd::Command;
    use serde_json::{Value, json};
    use std::{env, ffi::OsString, fs, path::Path};
    use tempfile::TempDir;

    struct EnvVarGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let original = env::var_os(key);
            // Tests mutate process env in a scoped guard and restore it in Drop.
            unsafe { env::set_var(key, value) };
            EnvVarGuard { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(ref value) = self.original {
                // Restore environment variable to its original value.
                unsafe { env::set_var(self.key, value) };
            } else {
                // Remove temporary environment variable set for a test.
                unsafe { env::remove_var(self.key) };
            }
        }
    }

    fn qa_cli_command() -> Command {
        if let Ok(path) = env::var("CARGO_BIN_EXE_greentic-qa") {
            return Command::new(path);
        }

        let mut command = Command::new("cargo");
        command
            .arg("run")
            .arg("-q")
            .arg("-p")
            .arg("greentic-qa")
            .arg("--");
        command
    }

    use crate::builder::{GenerationInput, QuestionInput, build_bundle, write_bundle};
    use serde_json::from_str;

    #[test]
    fn parse_answer_boolean_accepts_yes() {
        let question = json!({ "type": "boolean", "required": true });
        assert_eq!(parse_answer(&question, "yes").unwrap(), Value::Bool(true));
    }

    #[test]
    fn parse_answer_integer_handles_numbers() {
        let question = json!({ "type": "integer" });
        assert_eq!(
            parse_answer(&question, "42").unwrap(),
            Value::Number(Number::from(42))
        );
    }

    #[test]
    fn parse_answer_enum_checks_choices() {
        let question = json!({
            "type": "enum",
            "choices": ["alpha", "beta"],
            "required": true
        });
        assert!(parse_answer(&question, "gamma").is_err());
        assert_eq!(
            parse_answer(&question, "alpha").unwrap(),
            Value::String("alpha".into())
        );
    }

    #[test]
    fn parse_answer_list_accepts_array() {
        let question = json!({
            "type": "list",
            "required": true,
            "list": {
                "fields": [
                    { "id": "name" },
                    { "id": "value" }
                ]
            }
        });
        let value = parse_answer(&question, r#"[{"name": "alpha", "value": "v1"}]"#).unwrap();
        assert!(value.is_array());
    }

    #[test]
    fn parse_answer_list_rejects_non_array() {
        let question = json!({
            "type": "list",
            "required": true,
            "list": {
                "fields": [
                    { "id": "name" }
                ]
            }
        });
        assert!(parse_answer(&question, r#"{"name": "alpha"}"#).is_err());
    }

    #[test]
    fn parse_answer_respects_defaults() {
        let question = json!({
            "type": "string",
            "default": "default-value",
            "required": true
        });
        assert_eq!(
            parse_answer(&question, "").unwrap(),
            Value::String("default-value".into())
        );
    }

    #[test]
    fn load_resolved_i18n_map_requires_flat_string_map() {
        let dir = TempDir::new().expect("temp dir");
        let valid = dir.path().join("valid.json");
        fs::write(&valid, r#"{"q1.title":"Naam"}"#).expect("write valid");
        let loaded = load_resolved_i18n_map(&valid).expect("flat map should load");
        assert_eq!(loaded.get("q1.title").map(String::as_str), Some("Naam"));

        let invalid = dir.path().join("invalid.json");
        fs::write(&invalid, r#"{"q1":{"title":"Naam"}}"#).expect("write invalid");
        assert!(load_resolved_i18n_map(&invalid).is_err());
    }

    const FIXTURE: &str = include_str!("../../../ci/fixtures/sample_form_generation.json");

    #[test]
    fn fixture_generates_bundle() {
        let input: GenerationInput =
            from_str(FIXTURE).expect("fixture should deserialize into GenerationInput");
        let bundle = build_bundle(&input).expect("bundle build should succeed");
        let temp_dir = TempDir::new().expect("temp dir");

        let bundle_dir =
            write_bundle(&bundle, &input, temp_dir.path()).expect("bundle write should succeed");

        let forms_dir = bundle_dir.join("forms");
        let flows_dir = bundle_dir.join("flows");
        let examples_dir = bundle_dir.join("examples");
        let schemas_dir = bundle_dir.join("schemas");

        assert!(forms_dir.exists() && forms_dir.join("smoke-form.form.json").exists());
        assert!(flows_dir.exists() && flows_dir.join("smoke-form.qaflow.json").exists());
        assert!(
            examples_dir.exists()
                && examples_dir
                    .join("smoke-form.answers.example.json")
                    .exists()
        );
        assert!(
            schemas_dir.exists() && schemas_dir.join("smoke-form.answers.schema.json").exists()
        );

        let spec_contents =
            fs::read_to_string(forms_dir.join("smoke-form.form.json")).expect("read spec file");
        let spec_value: Value = serde_json::from_str(&spec_contents).expect("spec file JSON");
        assert_eq!(spec_value["id"].as_str(), Some("smoke-form"));
    }

    #[test]
    fn default_validation_accepts_boolean_values() {
        assert!(ensure_default_matches_type(CliQuestionType::Boolean, "y", None).is_ok());
        assert!(ensure_default_matches_type(CliQuestionType::Boolean, "false", None).is_ok());
        assert!(ensure_default_matches_type(CliQuestionType::Boolean, "maybe", None).is_err());
    }

    #[test]
    fn default_validation_requires_numeric_defaults() {
        assert!(ensure_default_matches_type(CliQuestionType::Integer, "0", None).is_ok());
        assert!(ensure_default_matches_type(CliQuestionType::Integer, "1.5", None).is_err());
        assert!(ensure_default_matches_type(CliQuestionType::Number, "1.5", None).is_ok());
        assert!(ensure_default_matches_type(CliQuestionType::Number, "bad", None).is_err());
    }

    #[test]
    fn default_validation_checks_enum_choice() {
        let choices = vec!["one".into(), "two".into()];
        assert!(ensure_default_matches_type(CliQuestionType::Enum, "one", Some(&choices)).is_ok());
        assert!(
            ensure_default_matches_type(CliQuestionType::Enum, "three", Some(&choices)).is_err()
        );
    }

    #[test]
    fn validate_question_input_rejects_bad_boolean_default() {
        let question = QuestionInput {
            id: "bool".into(),
            kind: CliQuestionType::Boolean,
            title: "Bool".into(),
            description: None,
            required: true,
            default_value: Some("we".into()),
            choices: None,
            secret: false,
            list: None,
            visible_if: None,
            constraint: None,
            computed: None,
            computed_overridable: false,
        };
        assert!(validate_question_input(&question).is_err());
    }

    #[test]
    fn ensure_allowed_root_accepts_writable_paths_outside_allowed_roots() {
        let allowed_root = TempDir::new().expect("temp dir");
        let other_root = TempDir::new().expect("temp dir");
        let _guard = EnvVarGuard::set("QA_WIZARD_ALLOWED_ROOTS", allowed_root.path());
        assert!(ensure_allowed_root(other_root.path()).is_ok());
    }

    #[test]
    fn new_command_skips_advanced_prompts_when_not_selected()
    -> Result<(), Box<dyn std::error::Error>> {
        let workspace = assert_fs::TempDir::new().unwrap();
        let output_root = workspace.path().join("wizard-out");
        let answers = [
            "form-id",
            "Form Title",
            "",
            "",
            "",
            "",
            "question-id",
            "Question Title",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
        ];
        let stdin = format!("{}\n", answers.join("\n"));

        let mut cmd = qa_cli_command();
        cmd.arg("new")
            .arg("--out")
            .arg(&output_root)
            .write_stdin(stdin)
            .assert()
            .success();

        let spec_path = output_root
            .join("form-id")
            .join("forms")
            .join("form-id.form.json");
        let spec_json = fs::read_to_string(&spec_path)?;
        let spec: Value = serde_json::from_str(&spec_json)?;
        let question = &spec["questions"][0];
        assert_eq!(question["secret"].as_bool(), Some(false));
        assert!(question.get("visible_if").is_none());
        assert!(question.get("computed").is_none());

        Ok(())
    }
}
