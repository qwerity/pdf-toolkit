use axum::http::StatusCode;
use clap::Parser;
use pdf_toolkit::cli::{Cli, Command};
use serde::Deserialize;

use super::error::ApiError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobRequest {
    pub args: Vec<String>,
}

/// Validate with the existing CLI parser, never a shell or a second option schema.
pub fn validate(args: &[String]) -> Result<(), ApiError> {
    let cli =
        Cli::try_parse_from(std::iter::once("pdf-toolkit").chain(args.iter().map(String::as_str)))
            .map_err(|error| ApiError(StatusCode::BAD_REQUEST, error.to_string()))?;
    let missing_output = match cli.command {
        Command::Cut(args) => args.output.is_none(),
        Command::Render(args) => args.output.is_none(),
        Command::Text(args) => args.output.is_none(),
        _ => false,
    };
    if missing_output {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "HTTP file-producing requests require --output; use a separate directory per job"
                .into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(args: &[&str]) -> Result<(), ApiError> {
        validate(&args.iter().map(|value| (*value).into()).collect::<Vec<_>>())
    }

    #[test]
    fn all_existing_commands_are_available() {
        for args in [
            vec!["inspect", "a.pdf"],
            vec!["cut", "a.pdf", "-o", "out"],
            vec!["render", "a.pdf", "-o", "out", "--pages", "1-3"],
            vec!["text", "a.pdf", "-o", "out"],
            vec!["structured", "a.pdf"],
            vec!["vector", "a.pdf"],
            vec!["dossier", "a.pdf"],
            vec!["cadfix", "a.pdf", "--page", "1"],
            vec!["building", "a.pdf"],
            vec!["model", "a.pdf"],
            vec!["downsample", "a.pdf", "-o", "out.pdf"],
        ] {
            assert!(check(&args).is_ok(), "{args:?}");
        }
    }

    #[test]
    fn rejects_recursion_help_and_implicit_output() {
        for args in [
            vec!["serve"],
            vec!["--help"],
            vec!["render", "a.pdf"],
            vec!["text", "a.pdf"],
            vec!["cut", "a.pdf"],
            vec!["inspect", "a.pdf", "--scale", "2"],
        ] {
            assert!(check(&args).is_err(), "{args:?}");
        }
    }
}
