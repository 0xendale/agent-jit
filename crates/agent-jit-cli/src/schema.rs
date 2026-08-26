//! `agent-jit schema` — generate the checked-in contracts, or validate one stored document.

use std::fmt::Write as _;
use std::path::Path;

use agent_jit_domain::schema::{generated_schemas, validate_document};

use crate::error::CommandError;
use crate::output::Rendered;

const USAGE: &str = "usage: agent-jit schema <generate --out <dir> | validate <file>>";

/// Dispatches a `schema` subcommand.
///
/// # Errors
///
/// Returns a [`CommandError`] when arguments are missing or the document is refused.
pub fn run(args: &[String]) -> Result<Rendered, CommandError> {
    match args.first().map(String::as_str) {
        Some("generate") => generate(&args[1..]),
        Some("validate") => validate(&args[1..]),
        Some(other) => Err(CommandError::usage(format!(
            "unknown schema subcommand: {other}\n{USAGE}"
        ))),
        None => Err(CommandError::usage(USAGE)),
    }
}

/// Writes every generated schema into the requested directory.
fn generate(args: &[String]) -> Result<Rendered, CommandError> {
    let out = match args {
        [flag, directory] if flag == "--out" => directory.clone(),
        _ => {
            return Err(CommandError::usage(
                "usage: agent-jit schema generate --out <dir>",
            ));
        }
    };

    let directory = Path::new(&out);
    std::fs::create_dir_all(directory)
        .map_err(|error| CommandError::refused("schema_unwritable", format!("{out}: {error}")))?;

    let schemas = generated_schemas();
    for schema in &schemas {
        let path = directory.join(&schema.file_name);
        std::fs::write(&path, &schema.contents).map_err(|error| {
            CommandError::refused("schema_unwritable", format!("{}: {error}", path.display()))
        })?;
    }

    let mut rendered = String::new();
    let _ = writeln!(rendered, "wrote {} schemas to {out}", schemas.len());
    for schema in &schemas {
        let _ = writeln!(rendered, "  {} v{}", schema.schema_name, schema.version);
    }
    Ok(Rendered::Text(rendered))
}

/// Validates one stored document against the contract it claims to implement.
fn validate(args: &[String]) -> Result<Rendered, CommandError> {
    let [path] = args else {
        return Err(CommandError::usage(
            "usage: agent-jit schema validate <file>",
        ));
    };

    let text = std::fs::read_to_string(path)
        .map_err(|error| CommandError::refused("schema_unreadable", format!("{path}: {error}")))?;
    let document: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
        CommandError::refused("schema_malformed_json", format!("{path}: {error}"))
    })?;

    let validated = validate_document(&document)
        .map_err(|error| CommandError::refused(error.code(), error.to_string()))?;

    Ok(Rendered::Text(format!(
        "{} v{} {} {}\n",
        validated.schema_name, validated.version, validated.id, validated.digest
    )))
}
