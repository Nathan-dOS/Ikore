//! Diagnostic parsers for the supported presets (§2, "bounded normalized
//! host diagnostics"). Each parser turns raw tool output into
//! `DiagnosticV1` rows with a stable code, a message and, when the tool
//! named a file the plan knows, its handle. Anything the parser cannot
//! attribute stays a diagnostic without `sourceRef`; a nonzero exit with no
//! parsed diagnostic still yields one, so a failure is never silent.

use super::super::{CheckKind, DiagnosticV1, Handle, MAX_TEXT_CHARS, Severity};

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

/// Interpretation of the supported Cargo JSON and libtest protocols. This
/// deliberately does not infer process success: the runner also checks the
/// exit status, stream completeness and input integrity.
#[derive(Debug, Clone)]
pub struct CargoInterpretation {
    pub diagnostics: Vec<DiagnosticV1>,
    pub interpreted: bool,
    pub build_succeeded: Option<bool>,
    pub tests_passed: Option<u64>,
    pub tests_failed: Option<u64>,
    pub tests_ignored: Option<u64>,
    pub limitations: Vec<String>,
}

/// Interpret only the stable Cargo protocol and the standard libtest text
/// grammar. Unknown harness output is a limitation, never an empty success.
pub fn interpret_cargo_output(
    kind: CheckKind,
    stdout: &str,
    stderr: &str,
    resolver: &dyn PathResolver,
) -> CargoInterpretation {
    let mut diagnostics = parse_cargo_json(stdout, resolver);
    let mut result = CargoInterpretation {
        diagnostics: Vec::new(),
        interpreted: true,
        build_succeeded: None,
        tests_passed: None,
        tests_failed: None,
        tests_ignored: None,
        limitations: Vec::new(),
    };
    let mut pending_tests = None;
    let mut summaries = 0u64;
    let mut counts = [0u64; 3];
    let mut failure_output = false;
    let mut observed_failure = false;
    for line in stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if line.starts_with('{') {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                limitation(&mut result, "Cargo emitted malformed JSON.");
                continue;
            };
            match value["reason"].as_str() {
                Some("build-finished") => match value["success"].as_bool() {
                    Some(success) if result.build_succeeded.is_none() => {
                        result.build_succeeded = Some(success);
                    }
                    _ => limitation(
                        &mut result,
                        "Cargo build completion is invalid or repeated.",
                    ),
                },
                Some("compiler-message") => {
                    if !value["message"]["level"].is_string()
                        || !value["message"]["message"].is_string()
                        || !value["message"]["spans"].is_array()
                    {
                        limitation(&mut result, "Cargo emitted an invalid compiler message.");
                    }
                }
                Some("compiler-artifact" | "build-script-executed") => {}
                _ => limitation(&mut result, "Cargo emitted an unsupported JSON message."),
            }
            continue;
        }
        if kind != CheckKind::Test {
            limitation(
                &mut result,
                "Cargo emitted output outside its supported JSON protocol.",
            );
            continue;
        }
        if let Some(total) = running_tests(line) {
            if pending_tests.replace(total).is_some() {
                limitation(
                    &mut result,
                    "A libtest run has no matching completion summary.",
                );
            }
            failure_output = false;
            observed_failure = false;
            continue;
        }
        if line.starts_with("test result:") {
            let Some(summary) = test_summary(line) else {
                limitation(
                    &mut result,
                    "The libtest completion summary is not understood.",
                );
                continue;
            };
            let total = summary[..4]
                .iter()
                .try_fold(0u64, |sum, count| sum.checked_add(*count));
            if total.is_none() || pending_tests.take() != total {
                limitation(
                    &mut result,
                    "Libtest counts do not match the announced test run.",
                );
            }
            for (count, additional) in counts.iter_mut().zip(summary) {
                if let Some(sum) = count.checked_add(additional) {
                    *count = sum;
                } else {
                    limitation(&mut result, "Libtest counts exceeded the supported range.");
                }
            }
            summaries += 1;
            failure_output = false;
            observed_failure = false;
            continue;
        }
        if line.starts_with("test ")
            && let Some((_, status)) = line.rsplit_once(" ... ")
            && (matches!(status, "ok" | "FAILED" | "ignored") || status.starts_with("ignored, "))
        {
            observed_failure |= status == "FAILED";
            continue;
        }
        if line == "failures:" && observed_failure {
            failure_output = true;
            continue;
        }
        if failure_output {
            // Captured assertion/panic text can be arbitrary. It is retained
            // only as bounded sanitized diagnostics by the existing parser;
            // matching libtest run/summary counts are still required below.
            continue;
        }
        if line.starts_with("all doctests ran in ")
            && line.contains("merged doctests compilation took ")
        {
            continue;
        }
        limitation(
            &mut result,
            "The test harness emitted output the configured parser does not support.",
        );
    }
    if result.build_succeeded.is_none() {
        limitation(&mut result, "Cargo did not report build completion.");
    }
    if kind == CheckKind::Test {
        let mut combined = stdout.to_owned();
        combined.push('\n');
        combined.push_str(stderr);
        diagnostics.extend(parse_cargo_test(&combined, resolver));
        if pending_tests.is_some() {
            limitation(
                &mut result,
                "A libtest run has no matching completion summary.",
            );
        }
        if result.build_succeeded == Some(true) && summaries == 0 {
            limitation(
                &mut result,
                "No supported libtest completion summary was observed.",
            );
        }
        if summaries > 0 {
            result.tests_passed = Some(counts[0]);
            result.tests_failed = Some(counts[1]);
            result.tests_ignored = Some(counts[2]);
        }
    }
    let mut failed_targets = 0usize;
    for line in stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Some(count) = failed_target_count(line) {
            if failed_targets != 0 || kind != CheckKind::Test {
                limitation(
                    &mut result,
                    "The failed-target summary is incomplete or unexpected.",
                );
            }
            failed_targets = count;
            continue;
        }
        if failed_targets > 0 && failed_target_selector(line) {
            failed_targets -= 1;
            continue;
        }
        if !cargo_stderr_line(line) {
            limitation(
                &mut result,
                "Cargo stderr contains output the configured parser does not support.",
            );
        }
    }
    if failed_targets > 0 {
        limitation(
            &mut result,
            "The failed-target summary is incomplete or unexpected.",
        );
    }
    diagnostics.truncate(MAX_DIAGNOSTICS);
    result.diagnostics = diagnostics;
    result
}

fn limitation(result: &mut CargoInterpretation, message: &str) {
    result.interpreted = false;
    if !result
        .limitations
        .iter()
        .any(|existing| existing == message)
    {
        result.limitations.push(message.to_owned());
    }
}

fn running_tests(line: &str) -> Option<u64> {
    let value = line.strip_prefix("running ")?;
    value
        .strip_suffix(" tests")
        .or_else(|| value.strip_suffix(" test"))?
        .parse()
        .ok()
}

/// Counts are passed, failed, ignored, measured and filtered out.
fn test_summary(line: &str) -> Option<[u64; 5]> {
    let rest = line.strip_prefix("test result: ")?;
    let (status, rest) = rest.split_once(". ")?;
    if !matches!(status, "ok" | "FAILED") {
        return None;
    }
    let mut fields = rest.split(';').map(str::trim);
    let mut counts = [0u64; 5];
    for (count, label) in
        counts
            .iter_mut()
            .zip(["passed", "failed", "ignored", "measured", "filtered out"])
    {
        let field = fields.next()?;
        let (number, actual_label) = field.split_once(' ')?;
        if actual_label != label {
            return None;
        }
        *count = number.parse().ok()?;
    }
    if (status == "ok") != (counts[1] == 0)
        || !fields.next()?.starts_with("finished in ")
        || fields.next().is_some()
    {
        return None;
    }
    Some(counts)
}

fn cargo_stderr_line(line: &str) -> bool {
    [
        "Compiling ",
        "Checking ",
        "Finished ",
        "Running ",
        "Doc-tests ",
        "warning:",
        "error:",
        "error[",
        "note:",
        "Caused by:",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
}

fn failed_target_count(line: &str) -> Option<usize> {
    let line = line.strip_prefix("error: ")?;
    let (count, label) = line.split_once(' ')?;
    let count = count.parse::<usize>().ok()?;
    ((count == 1 && label == "target failed:") || (count > 1 && label == "targets failed:"))
        .then_some(count)
}

fn failed_target_selector(line: &str) -> bool {
    let Some(selector) = line
        .strip_prefix('`')
        .and_then(|line| line.strip_suffix('`'))
    else {
        return false;
    };
    let mut parts = selector.split_whitespace();
    let Some(mut target) = parts.next() else {
        return false;
    };
    if target == "-p" {
        let Some(package) = parts.next() else {
            return false;
        };
        if package.starts_with('-') || package.contains('`') {
            return false;
        }
        let Some(next) = parts.next() else {
            return false;
        };
        target = next;
    }
    if matches!(
        target,
        "--lib" | "--doc" | "--bins" | "--tests" | "--examples"
    ) {
        return parts.next().is_none();
    }
    if matches!(target, "--bin" | "--test" | "--example") {
        return parts
            .next()
            .is_some_and(|name| !name.starts_with('-') && !name.contains('`'))
            && parts.next().is_none();
    }
    false
}

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
            let file = source_path_from_location(&location).to_owned();
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
            let file = source_path_from_location(rest);
            last.source_ref = resolver.handle_for(file);
            last.message = bounded(&format!("{} ({rest})", last.message));
        }
    }
    out
}

fn source_path_from_location(location: &str) -> &str {
    // Split line/column from the right so `C:\\project\\src\\lib.rs`
    // retains its drive prefix instead of being attributed to `C`.
    let location = location.trim().trim_end_matches(':');
    let Some((without_column, column)) = location.rsplit_once(':') else {
        return location;
    };
    if column.parse::<u64>().is_err() {
        return location;
    }
    if let Some((path, line)) = without_column.rsplit_once(':')
        && line.parse::<u64>().is_ok()
    {
        return path;
    }
    without_column
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

    const FINISHED: &str = "{\"reason\":\"build-finished\",\"success\":true}\n";

    #[test]
    fn qualified_cargo_requires_completion_and_understood_output() {
        let complete = interpret_cargo_output(CheckKind::Compile, FINISHED, "", &resolver);
        assert!(complete.interpreted);
        assert_eq!(complete.build_succeeded, Some(true));
        assert_eq!(complete.tests_passed, None);
        let missing = interpret_cargo_output(CheckKind::Compile, "", "", &resolver);
        assert!(!missing.interpreted);
        let malformed = format!("{{bad json}}\n{FINISHED}");
        assert!(!interpret_cargo_output(CheckKind::Compile, &malformed, "", &resolver).interpreted);
        let custom = format!("I checked everything\n{FINISHED}");
        assert!(!interpret_cargo_output(CheckKind::Compile, &custom, "", &resolver).interpreted);
    }

    #[test]
    fn qualified_libtest_counts_multiple_harnesses_and_zero_tests() {
        let stdout = format!(
            "{FINISHED}running 2 tests\ntest a ... ok\ntest b ... ignored, platform\n\
             test result: ok. 1 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\
             running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\
             running 1 test\ntest src/lib.rs - value (line 1) ... ok\n\
             test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\
             all doctests ran in 0.33s; merged doctests compilation took 0.32s\n"
        );
        let result = interpret_cargo_output(
            CheckKind::Test,
            &stdout,
            "Running unittests src/lib.rs (target/debug/deps/x)\nDoc-tests x",
            &resolver,
        );
        assert!(result.interpreted, "{:?}", result.limitations);
        assert_eq!(result.tests_passed, Some(2));
        assert_eq!(result.tests_failed, Some(0));
        assert_eq!(result.tests_ignored, Some(1));
        let zero = format!(
            "{FINISHED}running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"
        );
        assert_eq!(
            interpret_cargo_output(CheckKind::Test, &zero, "", &resolver).tests_passed,
            Some(0)
        );
    }

    #[test]
    fn qualified_libtest_rejects_incomplete_mismatched_and_custom_harnesses() {
        let incomplete = format!("{FINISHED}running 2 tests\ntest a ... ok\n");
        assert!(!interpret_cargo_output(CheckKind::Test, &incomplete, "", &resolver).interpreted);
        let mismatch = format!(
            "{FINISHED}running 2 tests\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"
        );
        assert!(!interpret_cargo_output(CheckKind::Test, &mismatch, "", &resolver).interpreted);
        let custom = format!("{FINISHED}custom runner says all good\n");
        assert!(!interpret_cargo_output(CheckKind::Test, &custom, "", &resolver).interpreted);
        assert!(!interpret_cargo_output(CheckKind::Test, FINISHED, "", &resolver).interpreted);
    }

    #[test]
    fn qualified_failed_build_is_distinct_from_failed_libtest() {
        let compiler = concat!(
            "{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",\"code\":{\"code\":\"E0308\"},\"message\":\"wrong type\",\"spans\":[]}}\n",
            "{\"reason\":\"build-finished\",\"success\":false}\n"
        );
        let result = interpret_cargo_output(
            CheckKind::Test,
            compiler,
            "error: could not compile x",
            &resolver,
        );
        assert!(result.interpreted);
        assert_eq!(result.build_succeeded, Some(false));
        assert_eq!(result.tests_failed, None);
        assert_eq!(result.diagnostics[0].code.as_str(), "e0308");
        let tests = format!(
            "{FINISHED}running 1 test\ntest a ... FAILED\nfailures:\n---- a stdout ----\nthread 'a' panicked at C:\\project\\src\\lib.rs:3:5:\nassertion failed\nfailures:\n a\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"
        );
        let result = interpret_cargo_output(
            CheckKind::Test,
            &tests,
            "error: test failed, to rerun pass `--lib`",
            &resolver,
        );
        assert!(result.interpreted, "{:?}", result.limitations);
        assert_eq!(result.tests_failed, Some(1));
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code.as_str() == "test_failed_a" && d.source_ref.is_some())
        );
    }

    #[test]
    fn qualified_no_fail_fast_understands_cargo_failed_target_lists() {
        // Cargo 1.98.0 with two workspace libraries failing under
        // --no-fail-fast puts a final selector list on stderr.
        let harness = "running 1 test\ntest fails ... FAILED\nfailures:\n---- fails stdout ----\nthread 'fails' panicked at src/lib.rs:1:1:\nassertion failed\nfailures:\n    fails\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
        let stdout = format!("{FINISHED}{harness}{harness}");
        let stderr = "Compiling parser-a v0.1.0\nFinished `test` profile\nRunning unittests src/lib.rs (target/debug/deps/parser_a)\nerror: test failed, to rerun pass `-p parser-a --lib`\nRunning unittests src/lib.rs (target/debug/deps/parser_b)\nerror: test failed, to rerun pass `-p parser-b --lib`\nDoc-tests parser_a\nDoc-tests parser_b\nerror: 2 targets failed:\n    `-p parser-a --lib`\n    `-p parser-b --lib`\n";
        let result = interpret_cargo_output(CheckKind::Test, &stdout, stderr, &resolver);
        assert!(result.interpreted, "{:?}", result.limitations);
        assert_eq!(result.tests_failed, Some(2));
        let one = format!("{FINISHED}{harness}");
        assert!(
            interpret_cargo_output(
                CheckKind::Test,
                &one,
                "error: 1 target failed:\n    `--lib`\n",
                &resolver
            )
            .interpreted
        );
        assert!(
            !interpret_cargo_output(
                CheckKind::Test,
                &one,
                "error: 2 targets failed:\n    `--lib`\n",
                &resolver
            )
            .interpreted
        );
        assert!(
            !interpret_cargo_output(
                CheckKind::Test,
                &one,
                "error: 1 target failed:\n    `cargo run`\n",
                &resolver
            )
            .interpreted
        );
        assert!(!interpret_cargo_output(CheckKind::Test, &one, "`--lib`\n", &resolver).interpreted);
    }
}
