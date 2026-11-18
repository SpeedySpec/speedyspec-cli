//! Asking the user questions.
//!
//! Behind a trait so the install flow can be tested without a terminal. Prompts are written to
//! stderr: stdout is for command output only.

use std::fmt;
use std::io::{BufRead, IsTerminal, Write};

use clap::ArgMatches;

/// Why a question got no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptError(pub String);

impl fmt::Display for PromptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Asks the user for a choice or a value.
pub trait Prompter {
    /// One of `choices`, each `(id, label)`; returns the chosen id.
    fn choose(&mut self, label: &str, choices: &[(String, String)]) -> Result<String, PromptError>;

    /// A line of text. An empty answer returns `default` when there is one, and an empty string
    /// otherwise. Secret input is not echoed.
    fn text(
        &mut self,
        label: &str,
        default: Option<&str>,
        secret: bool,
    ) -> Result<String, PromptError>;
}

/// Whether the CLI may ask questions: only when the user did not forbid it and there is a
/// person at the other end. A script or CI job has no terminal, and waiting on it would hang.
pub fn interactive(no_interaction: bool, stdin_is_terminal: bool) -> bool {
    !no_interaction && stdin_is_terminal
}

/// Prompts on stderr and reads answers from stdin.
pub struct TerminalPrompter;

impl TerminalPrompter {
    fn read_line(&self) -> Result<String, PromptError> {
        let mut line = String::new();
        let read = std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| PromptError(format!("could not read your answer: {e}")))?;
        if read == 0 {
            return Err(PromptError(
                "input ended before an answer was given".to_owned(),
            ));
        }
        Ok(line.trim().to_owned())
    }
}

impl Prompter for TerminalPrompter {
    fn choose(&mut self, label: &str, choices: &[(String, String)]) -> Result<String, PromptError> {
        loop {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err, "{label}:");
            for (index, (id, name)) in choices.iter().enumerate() {
                let _ = writeln!(err, "  {}) {id}  {name}", index + 1);
            }
            let _ = write!(err, "Choose 1-{}: ", choices.len());
            let _ = err.flush();
            drop(err);
            let answer = self.read_line()?;
            let by_number = answer
                .parse::<usize>()
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|i| choices.get(i));
            let by_id = choices.iter().find(|(id, _)| *id == answer);
            if let Some((id, _)) = by_number.or(by_id) {
                return Ok(id.clone());
            }
        }
    }

    fn text(
        &mut self,
        label: &str,
        default: Option<&str>,
        secret: bool,
    ) -> Result<String, PromptError> {
        let question = match default {
            Some(default) if !secret => format!("{label} [{default}]: "),
            _ => format!("{label}: "),
        };
        let answer = if secret {
            // Written to and read from the terminal itself, with echo off.
            rpassword::prompt_password(&question)
                .map_err(|e| PromptError(format!("could not read your answer: {e}")))?
        } else {
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "{question}");
            let _ = err.flush();
            drop(err);
            self.read_line()?
        };
        if answer.is_empty() {
            return Ok(default.unwrap_or_default().to_owned());
        }
        Ok(answer)
    }
}

/// Asks the user to confirm unless `--yes` was given. `Err` is the exit code.
pub fn confirm(matches: &ArgMatches, question: &str) -> Result<(), i32> {
    if matches.get_flag("yes") {
        return Ok(());
    }
    if !interactive(
        matches.get_flag("no-interaction"),
        std::io::stdin().is_terminal(),
    ) {
        eprintln!("speedyspec: confirm with --yes");
        return Err(2);
    }
    match TerminalPrompter.text(question, None, false) {
        Ok(answer) if matches!(answer.to_lowercase().as_str(), "y" | "yes") => Ok(()),
        Ok(_) => {
            eprintln!("speedyspec: cancelled");
            Err(1)
        }
        Err(error) => {
            eprintln!("speedyspec: {error}");
            Err(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_needs_a_terminal_and_no_flag() {
        assert!(interactive(false, true));
        assert!(!interactive(true, true));
        assert!(!interactive(false, false));
        assert!(!interactive(true, false));
    }
}
