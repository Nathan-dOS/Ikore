//! Diagnostic parsers for the supported presets (§2, "bounded normalized
//! host diagnostics"). Each parser turns raw tool output into
//! `DiagnosticV1` rows with a stable code, a message and, when the tool
//! named a file the plan knows, its handle. Anything the parser cannot
//! attribute stays a diagnostic without `sourceRef`; a nonzero exit with no
//! parsed diagnostic still yields one, so a failure is never silent.

use super::super::{DiagnosticV1, Handle, MAX_TEXT_CHARS, Severity};

/// Maps a tool-reported path (relative to the candidate or absolute inside
/// it) to a plan handle.
pub trait PathResolver: Sync {
    fn handle_for(&self, path: &str) -> Option<Handle>;
}

impl<F: Fn(&str) -> Option<Handle> + Sync> PathResolver for F {
    fn handle_for(&self, path: &str) -> Option<Handle> {
        self(path)
    }
}

/// Bounded number of diagnostics kept per receipt (contract: ≤ 64).
pub const MAX_DIAGNOSTICS: usize = 64;

fn bounded(text: &str) -> String {
    // Decode structured tool fields first, sanitize second, then bound text.
    let sanitized = crate::secret_redactor::redact_secrets(text)
        .map(|result| result.text)
        .unwrap_or_else(|_| "Validation detail withheld: sanitization unavailable.".into());
    let trimmed = sanitized.trim();
    if trimmed.chars().count() > MAX_TEXT_CHARS {
        let mut out: String = trimmed.chars().take(MAX_TEXT_CHARS - 3).collect();
        out.push_str("...");
        out
    } else {
        trimmed.to_owned()
    }
}

fn code(text: &str) -> Handle {
    if crate::secret_redactor::redact_secrets(text).map_or(true, |result| result.count > 0) {
        return Handle::new("diagnostic").expect("built-in diagnostic code is a valid handle");
    }
    let mut slug = String::with_capacity(text.len());
    for ch in text.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            slug.push(lower);
        } else if !slug.ends_with('_') && !slug.is_empty() {
            slug.push('_');
        }
    }
    let slug = slug.trim_matches('_').to_owned();
    let slug = if slug.is_empty() || !slug.starts_with(|c: char| c.is_ascii_lowercase()) {
        format!("d_{slug}")
    } else {
        slug
    };
    let slug: String = slug.chars().take(64).collect();
    Handle::new(slug.trim_end_matches('_')).unwrap_or_else(|_| {
        Handle::new("diagnostic").expect("built-in diagnostic code is a valid handle")
    })
}

/// `cargo --message-format=json`: one JSON object per line; compiler
/// messages carry level, code and primary span.
pub fn parse_cargo_json(stdout: &str, resolver: &dyn PathResolver) -> Vec<DiagnosticV1> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        if out.len() >= MAX_DIAGNOSTICS {
            break;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value["reason"] != "compiler-message" {
            continue;
        }
        let message = &value["message"];
        let severity = match message["level"].as_str() {
            Some("error") | Some("error: internal compiler error") => Severity::Error,
            Some("warning") => Severity::Warning,
            _ => continue,
        };
        let code_text = message["code"]["code"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if severity == Severity::Error {
                    "rustc_error".into()
                } else {
                    "rustc_warning".into()
                }
            });
        let primary = message["spans"]
            .as_array()
            .and_then(|spans| {
                spans
                    .iter()
                    .find(|span| span["is_primary"] == true)
                    .or_else(|| spans.first())
            })
            .cloned();
        let location = primary.as_ref().and_then(|span| {
            let file = span["file_name"].as_str()?;
            let line = span["line_start"].as_u64().unwrap_or(0);
            Some((file.to_owned(), line))
        });
        let text = message["message"].as_str().unwrap_or("compiler message");
        let rendered = match &location {
            Some((file, line)) => format!("{file}:{line}: {text}"),
            None => text.to_owned(),
        };
        out.push(DiagnosticV1 {
            severity,
            code: code(&code_text),
            message: bounded(&rendered),
            source_ref: location
                .as_ref()
                .and_then(|(file, _)| resolver.handle_for(file)),
        });
    }
    out
}

/// `cargo test` human output: failed test names and the panic locations
/// the harness prints (`panicked at path:line:col`).
pub fn parse_cargo_test(output: &str, resolver: &dyn PathResolver) -> Vec<DiagnosticV1> {
    let mut out = Vec::new();
    for line in output.lines() {
        if out.len() >= MAX_DIAGNOSTICS {
            break;
        }
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("---- ")
            && let Some(name) = rest.strip_suffix(" stdout ----")
        {
            out.push(DiagnosticV1 {
                severity: Severity::Error,
                code: code(&format!("test_failed_{name}")),
                message: bounded(&format!("test `{name}` failed")),
                source_ref: None,
            });
            continue;
        }
        if let Some(index) = trimmed.find("panicked at ") {
            let location = trimmed[index + "panicked at ".len()..]
                .trim_end_matches(':')
                .to_owned();
            let file = location.split(':').next().unwrap_or_default().to_owned();
            if let Some(last) = out.last_mut()
                && last.source_ref.is_none()
            {
                last.message = bounded(&format!("{} at {location}", last.message));
                last.source_ref = resolver.handle_for(&file);
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("error")
            && (rest.starts_with('[') || rest.starts_with(':'))
        {
            out.push(DiagnosticV1 {
                severity: Severity::Error,
                code: code(
                    rest.strip_prefix('[')
                        .and_then(|r| r.split(']').next())
                        .unwrap_or("rustc_error"),
                ),
                message: bounded(trimmed),
                source_ref: None,
            });
        }
        if let Some(rest) = trimmed.strip_prefix("--> ")
            && let Some(last) = out.last_mut()
            && last.source_ref.is_none()
            && last.code.as_str() != "test_failed"
        {
            let file = rest.split(':').next().unwrap_or_default();
            last.source_ref = resolver.handle_for(file);
            last.message = bounded(&format!("{} ({rest})", last.message));
        }
    }
    out
}

/// Fallback for custom commands: `path(line,col): error CODE: text`
/// (tsc/MSBuild shape) and `path:line: error: text` (gcc shape); anything
/// else becomes one diagnostic per line starting with `error`.
pub fn parse_generic(output: &str, resolver: &dyn PathResolver) -> Vec<DiagnosticV1> {
    let mut out = Vec::new();
    for line in output.lines() {
        if out.len() >= MAX_DIAGNOSTICS {
            break;
        }
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        let Some(position) = lower.find("error") else {
            continue;
        };
        let head = trimmed[..position].trim().trim_end_matches(':').trim();
        let file = head
            .split('(')
            .next()
            .unwrap_or_default()
            .split(':')
            .next()
            .unwrap_or_default()
            .trim();
        let source_ref = if file.is_empty() {
            None
        } else {
            resolver.handle_for(file)
        };
        let tail = trimmed[position..]
            .trim_start_matches("error")
            .trim_start_matches("Error");
        let code_text = tail
            .trim_start()
            .split(|c: char| c == ':' || c.is_whitespace())
            .next()
            .filter(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_alphanumeric()))
            .unwrap_or("error");
        out.push(DiagnosticV1 {
            severity: Severity::Error,
            code: code(code_text),
            message: bounded(trimmed),
            source_ref,
        });
    }
    out
}

/// The single diagnostic a failed run gets when no parser produced one.
pub fn exit_diagnostic(exit_code: Option<i32>, stderr_tail: &str) -> DiagnosticV1 {
    let tail = stderr_tail
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    DiagnosticV1 {
        severity: Severity::Error,
        code: Handle::new("check_failed").expect("built-in diagnostic code is a valid handle"),
        message: bounded(&match exit_code {
            Some(code) if tail.is_empty() => format!("The command exited with status {code}."),
            Some(code) => format!("The command exited with status {code}: {tail}"),
            None => "The command did not return an exit status.".to_owned(),
        }),
        source_ref: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_diagnostics_are_sanitized_before_text_limits() {
        let encoded = r#"{"reason":"compiler-message","message":{"level":"error","code":null,"message":"credential sk-proj-\u0061bcdefghijklmnopqrst","spans":[]}}"#;
        let rows = parse_cargo_json(encoded, &|_: &str| None);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].message.contains("abcdefghijklmnopqrst"));
        assert!(rows[0].message.contains("<REDACTED_SECRET:"));
        let long = format!(
            "{}sk-proj-abcdefghijklmnopqrst",
            "a ".repeat(MAX_TEXT_CHARS / 2 - 8)
        );
        assert!(!bounded(&long).contains("sk-proj-"));
    }

    fn resolver(path: &str) -> Option<Handle> {
        (path.replace('\\', "/").ends_with("src/lib.rs")).then(|| Handle::new("src_1").unwrap())
    }

    #[test]
    fn cargo_json_messages_become_attributed_diagnostics() {
        let stdout = concat!(
            r#"{"reason":"compiler-artifact","target":{"name":"x"}}"#,
            "\n",
            r#"{"reason":"compiler-message","message":{"level":"error","code":{"code":"E0308"},"message":"mismatched types","spans":[{"file_name":"src/lib.rs","line_start":3,"is_primary":true}]}}"#,
            "\n",
            r#"{"reason":"compiler-message","message":{"level":"warning","code":null,"message":"unused variable","spans":[{"file_name":"src/other.rs","line_start":9,"is_primary":true}]}}"#,
            "\n",
            "not json\n",
        );
        let diagnostics = parse_cargo_json(stdout, &resolver);
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].severity, Severity::Error);
        assert_eq!(diagnostics[0].code.as_str(), "e0308");
        assert_eq!(diagnostics[0].message, "src/lib.rs:3: mismatched types");
        assert_eq!(
            diagnostics[0].source_ref.as_ref().map(|h| h.as_str()),
            Some("src_1")
        );
        assert_eq!(diagnostics[1].severity, Severity::Warning);
        assert_eq!(diagnostics[1].code.as_str(), "rustc_warning");
        assert!(diagnostics[1].source_ref.is_none());
    }

    #[test]
    fn cargo_test_output_names_failed_tests_and_panic_locations() {
        let output = "running 2 tests\ntest a ... ok\ntest b ... FAILED\n\nfailures:\n\n---- b stdout ----\nthread 'b' panicked at src/lib.rs:7:5:\nassertion failed\n\nfailures:\n    b\n";
        let diagnostics = parse_cargo_test(output, &resolver);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_str(), "test_failed_b");
        assert!(diagnostics[0].message.contains("src/lib.rs:7:5"));
        assert_eq!(
            diagnostics[0].source_ref.as_ref().map(|h| h.as_str()),
            Some("src_1")
        );
        let compile = "error[E0425]: cannot find value `x`\n --> tests/t.rs:3:5\n";
        let diagnostics = parse_cargo_test(compile, &resolver);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_str(), "e0425");
        assert!(diagnostics[0].message.contains("tests/t.rs:3:5"));
    }

    #[test]
    fn generic_output_and_bare_exits_still_yield_a_diagnostic() {
        let diagnostics = parse_generic(
            "src/lib.rs(4,2): error TS2322: wrong\nall good\n",
            &resolver,
        );
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_str(), "ts2322");
        assert_eq!(
            diagnostics[0].source_ref.as_ref().map(|h| h.as_str()),
            Some("src_1")
        );
        let exit = exit_diagnostic(Some(3), "first\nlast line\n\n");
        assert_eq!(exit.code.as_str(), "check_failed");
        assert_eq!(exit.message, "The command exited with status 3: last line");
    }
}
