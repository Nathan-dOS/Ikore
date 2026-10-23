//! Host-owned projection of secrets; coordinates always address immutable original bytes.
use regex::Regex;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, ops::Range, sync::OnceLock};

pub use hvc_contracts::redaction::{
    AuthorizedSpan, ContextRedaction, MAX_REDACTION_SPANS, MAX_SANITIZATION_BYTES, OMITTED_CONTENT,
    RedactionResult, RedactionSpan, SANITIZATION_POLICY_VERSION,
};

#[derive(Debug)]
pub struct SecretRedactor {
    patterns: Vec<RedactionPattern>,
    marker: Regex,
    pem_begin: Regex,
    yaml_block: Regex,
}
#[derive(Debug)]
struct RedactionPattern {
    id: &'static str,
    regex: Regex,
    capture: usize,
}
impl SecretRedactor {
    pub fn new() -> Result<Self, regex::Error> {
        let definitions = [
            (
                "bearer_token",
                r"(?i)\bBearer\s+([A-Za-z0-9._~+/=-]{8,})",
                1,
            ),
            (
                "jwt_token",
                r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]*",
                0,
            ),
            (
                "known_api_token",
                // GitLab's documented default prefixes identify token families, not
                // exact lengths. Require at least 12 payload characters to avoid
                // matching short labels/examples; this does not validate a token.
                // https://docs.gitlab.com/security/tokens/#token-prefixes
                r"\b(?:sk-(?:proj-|ant-)?[A-Za-z0-9_-]{12,}|AIza[A-Za-z0-9_-]{20,}|gh[pousr]_[A-Za-z0-9_.-]{12,}|github_pat_[A-Za-z0-9_]{12,}|xox[baprs]-[A-Za-z0-9-]{12,}|gl(?:pat|oas|dt|rt|rtr|cbt|ptt|ft|imt|agent|wt|soat|ffct)-[A-Za-z0-9_-]{12,}|(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{12,}|SG\.[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{24,}|(?:AKIA|ASIA)[A-Z0-9]{16})\b",
                0,
            ),
            (
                "known_api_token",
                // The session cookie has an exact key and a separately bounded
                // cookie-value alphabet; retain the key, redact only its value.
                r"\b_gitlab_session=([A-Za-z0-9%_~+./=-]{16,})",
                1,
            ),
            (
                "sensitive_variable",
                r#"(?im)\b(?:[A-Z][A-Z0-9_]*_)?(?:PASSWORD|PASSWD|SECRET|TOKEN|API_KEY|API_SECRET|ACCESS_KEY|ACCESS_KEY_ID|SECRET_ACCESS_KEY|SESSION_TOKEN|PRIVATE_KEY|AUTH_KEY|AUTH_TOKEN|SIGNING_KEY|ENCRYPTION_KEY|CREDENTIALS?|CLIENT_SECRET)["']?\s*[:=]\s*["']?([^\s"',;<>]+)"#,
                1,
            ),
            (
                "credential_url",
                r"(?i)\b[a-z][a-z0-9+.-]*://[^/\s:@]+:([^/\s@]+)@",
                1,
            ),
            (
                "credential_path",
                r"(?i)(?:[A-Z]:\\|/)(?:[^\s\r\n]+[/\\])?(?:\.ssh[/\\]id_(?:rsa|ed25519)(?:\.pub)?|\.aws[/\\]credentials|application_default_credentials\.json|credentials\.(?:json|ya?ml|toml))",
                0,
            ),
        ];
        let mut patterns = definitions
            .into_iter()
            .map(|(id, expression, capture)| {
                Ok(RedactionPattern {
                    id,
                    regex: Regex::new(expression)?,
                    capture,
                })
            })
            .collect::<Result<Vec<_>, regex::Error>>()?;
        // Quoted assignments can contain whitespace and escaped quotes. Detect the
        // whole value on the original, then merge with shorter contextual matches.
        const NAME: &str = r"(?:[A-Z][A-Z0-9_]*_)?(?:PASSWORD|PASSWD|SECRET|TOKEN|API_KEY|API_SECRET|ACCESS_KEY|ACCESS_KEY_ID|SECRET_ACCESS_KEY|SESSION_TOKEN|PRIVATE_KEY|AUTH_KEY|AUTH_TOKEN|SIGNING_KEY|ENCRYPTION_KEY|CREDENTIALS?|CLIENT_SECRET)";
        for expression in [
            format!(r#"(?im)\b{NAME}["']?\s*[:=]\s*"((?:\\.|[^"\\])*)""#),
            format!(r#"(?im)\b{NAME}["']?\s*[:=]\s*'((?:\\.|[^'\\])*)'"#),
        ] {
            patterns.push(RedactionPattern {
                id: "sensitive_variable",
                regex: Regex::new(&expression)?,
                capture: 1,
            });
        }
        Ok(Self {
            patterns,
            marker: Regex::new(
                r"<REDACTED_SECRET:(?:PRIVATE_KEY|BEARER_TOKEN|JWT_TOKEN|KNOWN_API_TOKEN|SENSITIVE_VARIABLE|CREDENTIAL_URL|CREDENTIAL_PATH|API_KEY|DB_PASSWORD)_[0-9]+>",
            )?,
            pem_begin: Regex::new(r"-----BEGIN ([A-Z0-9 ]*PRIVATE KEY)-----")?,
            // YAML block indentation is resolved on the original lines below.
            // The header alone cannot bound a literal/folded multiword value.
            yaml_block: Regex::new(&format!(
                r#"(?im)^[ \t]*(?:-[ \t]+)?(?P<key>["']?{NAME}["']?)[ \t]*:[ \t]*(?P<indicator>[|>](?:[1-9][+-]?|[+-][1-9]?)?)[ \t]*(?:#[^\r\n]*)?\r?$"#
            ))?,
        })
    }
    pub fn scan(&self, input: &str) -> Result<Vec<RedactionSpan>, String> {
        if input.len() > MAX_SANITIZATION_BYTES {
            return Err("sanitization_input_limit".into());
        }
        let markers: Vec<_> = self.marker.find_iter(input).map(|m| m.range()).collect();
        let mut found: Vec<(Range<usize>, &'static str)> = Vec::new();
        let mut yaml_end = 0;
        for capture in self.yaml_block.captures_iter(input) {
            let header = capture.get(0).expect("whole YAML header");
            if header.start() < yaml_end {
                continue;
            }
            let key = capture.name("key").expect("YAML key");
            let indicator = capture.name("indicator").expect("YAML indicator");
            let parent_indent = key.start() - header.start();
            let explicit_indent = indicator
                .as_str()
                .bytes()
                .find(u8::is_ascii_digit)
                .map(|digit| parent_indent + usize::from(digit - b'0'));
            let mut body_indent = explicit_indent;
            let body_start =
                header.end() + usize::from(input.as_bytes().get(header.end()) == Some(&b'\n'));
            let mut end = header.end();
            let mut offset = body_start;
            for line in input[body_start..].split_inclusive('\n') {
                let content = line.trim_end_matches(['\r', '\n']);
                let indent = content
                    .bytes()
                    .take_while(|byte| matches!(*byte, b' ' | b'\t'))
                    .count();
                if !content[indent..].is_empty() {
                    if indent <= parent_indent
                        || body_indent.is_some_and(|required| indent < required)
                    {
                        break;
                    }
                    body_indent.get_or_insert(indent);
                }
                // Include blank lines even at parent indentation. A nonblank
                // dedented field/comment is the boundary and is left intact.
                offset += line.len();
                end = offset;
            }
            found.push((indicator.start()..end, "sensitive_variable"));
            yaml_end = end;
            if found.len() > MAX_REDACTION_SPANS {
                return Err("sanitization_span_limit".into());
            }
        }
        let mut cursor = 0;
        while let Some(capture) = self.pem_begin.captures(&input[cursor..]) {
            let begin = capture.get(0).expect("whole match");
            let start = cursor + begin.start();
            let body = cursor + begin.end();
            let closing = format!("-----END {}-----", &capture[1]);
            let end = input[body..]
                .find(&closing)
                .map_or(input.len(), |n| body + n + closing.len());
            found.push((start..end, "private_key"));
            if found.len() > MAX_REDACTION_SPANS {
                return Err("sanitization_span_limit".into());
            }
            cursor = end;
            if cursor == input.len() {
                break;
            }
        }
        let private_regions: Vec<_> = found
            .iter()
            .filter(|(_, category)| *category == "private_key")
            .map(|(r, _)| r.clone())
            .collect();
        for pattern in &self.patterns {
            for captures in pattern.regex.captures_iter(input) {
                let Some(m) = captures.get(pattern.capture) else {
                    continue;
                };
                let range = m.range();
                if range.is_empty() {
                    continue;
                }
                let marker_index = markers.partition_point(|s| s.end <= range.start);
                if markers
                    .get(marker_index)
                    .is_some_and(|s| s.start <= range.start && s.end >= range.end)
                {
                    continue;
                }
                if pattern.id == "credential_path"
                    && m.as_str().to_ascii_lowercase().ends_with(".pub")
                {
                    continue;
                }
                let private_index = private_regions.partition_point(|s| s.end <= range.start);
                if private_regions
                    .get(private_index)
                    .is_some_and(|s| s.start <= range.start && s.end >= range.end)
                {
                    continue;
                }
                found.push((range, pattern.id));
                if found.len() > MAX_REDACTION_SPANS {
                    return Err("sanitization_span_limit".into());
                }
            }
        }
        found.sort_by(|(a, ac), (b, bc)| {
            a.start
                .cmp(&b.start)
                .then(b.end.cmp(&a.end))
                .then(ac.cmp(bc))
        });
        let mut consolidated: Vec<(Range<usize>, &'static str)> = Vec::new();
        for (range, category) in found {
            if let Some((previous, previous_category)) = consolidated.last_mut()
                && range.start < previous.end
            {
                if range.end - range.start > previous.end - previous.start {
                    *previous_category = category;
                }
                previous.end = previous.end.max(range.end);
            } else {
                consolidated.push((range, category));
            }
        }
        Ok(consolidated
            .into_iter()
            .enumerate()
            .map(|(index, (range, category))| RedactionSpan {
                start_byte: range.start,
                end_byte: range.end,
                detector_id: category.into(),
                detector_version: 1,
                category: category.into(),
                marker: format!(
                    "<REDACTED_SECRET:{}_{}>",
                    category.to_ascii_uppercase(),
                    index + 1
                ),
            })
            .collect())
    }
    pub fn redact_context(&self, input: &str) -> Result<ContextRedaction, String> {
        let spans = self.scan(input)?;
        let text = render_redacted_range(input, &spans, 0, input.len())?;
        Ok(ContextRedaction {
            text,
            spans,
            applied_exceptions: Vec::new(),
            authorized_spans: Vec::new(),
        })
    }
    /// Compatibility method. Overflow is represented as omission, never as original content.
    pub fn redact(&self, input: &str) -> RedactionResult {
        self.redact_context(input)
            .map(|r| r.result())
            .unwrap_or_else(|_| RedactionResult {
                text: OMITTED_CONTENT.into(),
                count: 0,
                categories: vec!["omitted".into()],
            })
    }
}
fn redactor() -> Result<&'static SecretRedactor, String> {
    static REDACTOR: OnceLock<Result<SecretRedactor, String>> = OnceLock::new();
    REDACTOR
        .get_or_init(|| {
            SecretRedactor::new().map_err(|_| "sanitization_initialization_failed".into())
        })
        .as_ref()
        .map_err(Clone::clone)
}
pub fn redact_context(input: &str) -> Result<ContextRedaction, String> {
    redactor()?.redact_context(input)
}
pub fn detect_secret_spans(input: &str) -> Result<Vec<RedactionSpan>, String> {
    redactor()?.scan(input)
}
pub fn redact_secrets(input: &str) -> Result<RedactionResult, String> {
    Ok(redact_context(input)?.result())
}
pub fn contains_redaction_marker(input: &str) -> bool {
    input.contains("<REDACTED_SECRET:")
}
pub fn introduces_redaction_marker(original: &str, updated: &str) -> bool {
    fn markers(input: &str) -> std::collections::BTreeMap<&str, usize> {
        let mut result = std::collections::BTreeMap::new();
        for (start, _) in input.match_indices("<REDACTED_SECRET:") {
            let tail = &input[start..];
            let marker = &tail[..tail.find('>').map_or(tail.len(), |end| end + 1)];
            *result.entry(marker).or_insert(0) += 1;
        }
        result
    }
    let old = markers(original);
    markers(updated)
        .iter()
        .any(|(marker, count)| old.get(marker).copied().unwrap_or(0) < *count)
}
pub fn text_sha256(input: &str) -> String {
    format!("{:x}", Sha256::digest(input.as_bytes()))
}

/// Redact intersections before inserting anchor delimiters or line numbers.
pub fn render_redacted_range(
    original: &str,
    spans: &[RedactionSpan],
    start: usize,
    end: usize,
) -> Result<String, String> {
    if start > end
        || end > original.len()
        || !original.is_char_boundary(start)
        || !original.is_char_boundary(end)
    {
        return Err("invalid_sanitization_range".into());
    }
    let mut text = String::new();
    let mut cursor = start;
    for span in spans {
        if span.start_byte >= end {
            break;
        }
        if span.end_byte <= start {
            continue;
        }
        let from = span.start_byte.max(start);
        let to = span.end_byte.min(end);
        if from < cursor || !original.is_char_boundary(from) || !original.is_char_boundary(to) {
            return Err("invalid_sanitization_map".into());
        }
        text.push_str(&original[cursor..from]);
        text.push_str(&span.marker);
        text.extend(
            original[from..to]
                .chars()
                .filter(|c| *c == '\n' || *c == '\r'),
        );
        cursor = to;
    }
    text.push_str(&original[cursor..end]);
    Ok(text)
}

/// Proof of preparation. Debug intentionally excludes the model's text.
#[derive(Clone)]
pub struct PreparedModelInput {
    system: String,
    user: String,
    identity: String,
    digest: String,
    count: u64,
    categories: Vec<String>,
    original_bytes: usize,
}
impl std::fmt::Debug for PreparedModelInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedModelInput")
            .field("policy", &self.identity)
            .field("digest", &self.digest)
            .field("count", &self.count)
            .finish()
    }
}
impl PreparedModelInput {
    pub fn new(system: &str, user: &str) -> Result<Self, String> {
        Self::with_authorized_ranges(system, user, SANITIZATION_POLICY_VERSION, &[], &[])
    }
    pub(crate) fn with_authorized_ranges(
        system: &str,
        user: &str,
        identity: &str,
        system_ranges: &[Range<usize>],
        user_ranges: &[Range<usize>],
    ) -> Result<Self, String> {
        fn project(input: &str, allowed: &[Range<usize>]) -> Result<ContextRedaction, String> {
            for range in allowed {
                if range.start >= range.end
                    || range.end > input.len()
                    || !input.is_char_boundary(range.start)
                    || !input.is_char_boundary(range.end)
                {
                    return Err("invalid_authorized_range".into());
                }
            }
            let mut projection = redact_context(input)?;
            projection.spans.retain(|span| {
                !allowed
                    .iter()
                    .any(|range| range.start <= span.start_byte && range.end >= span.end_byte)
            });
            projection.text = render_redacted_range(input, &projection.spans, 0, input.len())?;
            Ok(projection)
        }
        let original_bytes = system
            .len()
            .checked_add(user.len())
            .ok_or("sanitization_input_limit")?;
        if original_bytes > MAX_SANITIZATION_BYTES {
            return Err("sanitization_input_limit".into());
        }
        let system = project(system, system_ranges)?;
        let user = project(user, user_ranges)?;
        let count = (system.spans.len() + user.spans.len()) as u64;
        let categories = system
            .spans
            .iter()
            .chain(&user.spans)
            .map(|s| s.category.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let digest = Self::digest(identity, &system.text, &user.text);
        Ok(Self {
            system: system.text,
            user: user.text,
            identity: identity.into(),
            digest,
            count,
            categories,
            original_bytes,
        })
    }
    fn digest(identity: &str, system: &str, user: &str) -> String {
        text_sha256(&format!(
            "{}:{}{}:{}{}",
            identity.len(),
            identity,
            system.len(),
            system,
            user
        ))
    }
    pub fn system_prompt(&self) -> &str {
        &self.system
    }
    pub fn user_prompt(&self) -> &str {
        &self.user
    }
    pub fn policy_identity(&self) -> &str {
        &self.identity
    }
    pub fn redaction_count(&self) -> u64 {
        self.count
    }
    pub fn categories(&self) -> &[String] {
        &self.categories
    }
    pub fn original_bytes(&self) -> usize {
        self.original_bytes
    }
    pub fn matches(&self, system: &str, user: &str) -> bool {
        self.system == system && self.user == user
    }
    pub fn verify(&self) -> Result<(), String> {
        if Self::digest(&self.identity, &self.system, &self.user) == self.digest {
            Ok(())
        } else {
            Err("invalid_prepared_input".into())
        }
    }
    pub fn verify_matches(&self, system: &str, user: &str) -> Result<(), String> {
        self.verify()?;
        if self.matches(system, user) {
            Ok(())
        } else {
            Err("prepared_input_changed".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitlab_default_prefixes_and_session_cookie_require_a_bounded_payload() {
        for prefix in [
            "glpat-", "gloas-", "gldt-", "glrt-", "glrtr-", "glcbt-", "glptt-", "glft-", "glimt-",
            "glagent-", "glwt-", "glsoat-", "glffct-",
        ] {
            let token = format!("{prefix}synthetic_payload_123456");
            let input = format!("credential {token} end");
            let view = redact_context(&input).unwrap();
            assert_eq!(view.spans.len(), 1, "{prefix}");
            assert_eq!(view.spans[0].detector_id, "known_api_token");
            assert!(!view.text.contains(&token));
            for short in [
                prefix.to_owned(),
                format!("{prefix}example"),
                format!("{prefix}abcdefghijk"),
            ] {
                assert!(
                    redact_context(&short).unwrap().spans.is_empty(),
                    "short {prefix}"
                );
            }
        }
        let cookie =
            "Cookie: language=en; _gitlab_session=synthetic%2Fcookie_payload_123456; theme=dark";
        let view = redact_context(cookie).unwrap();
        assert_eq!(view.spans.len(), 1);
        assert_eq!(view.spans[0].detector_id, "known_api_token");
        assert!(!view.text.contains("cookie_payload"));
        assert!(
            view.text
                .starts_with("Cookie: language=en; _gitlab_session=")
        );
        assert!(view.text.ends_with("; theme=dark"));
        assert!(
            redact_context("_gitlab_session=short_example")
                .unwrap()
                .spans
                .is_empty()
        );
    }

    #[test]
    fn yaml_sensitive_block_values_are_bounded_before_projection() {
        for newline in ["\n", "\r\n"] {
            for indicator in ["|", ">", "|-", ">+", "|2-", ">-2", "|+2"] {
                let original = format!(
                    "config:{newline}  password: {indicator} # header comment{newline}{newline}    arbitrary multi word secret{newline}      continued value{newline}    # literal body comment{newline}  next: ok{newline}"
                );
                let view = redact_context(&original).unwrap();
                assert_eq!(view.spans.len(), 1, "{indicator}");
                assert_eq!(view.spans[0].detector_id, "sensitive_variable");
                for private in ["arbitrary", "continued value", "literal body comment"] {
                    assert!(!view.text.contains(private));
                }
                assert!(view.text.ends_with(&format!("  next: ok{newline}")));
                assert_eq!(
                    view.text.matches('\n').count(),
                    original.matches('\n').count()
                );
                assert_eq!(
                    view.text.matches('\r').count(),
                    original.matches('\r').count()
                );
                assert_eq!(redact_context(&view.text).unwrap().text, view.text);
            }
        }
        for original in [
            "password: >+\n\n  arbitrary multi word secret",
            "items:\n  - api_token: |-\n      arbitrary multi word secret\n    label: ok\n",
            "'client_secret': |2 # comment\n  arbitrary multi word secret\nnext: ok\n",
        ] {
            let view = redact_context(original).unwrap();
            assert!(!view.text.contains("arbitrary multi word secret"));
            assert_eq!(view.spans.len(), 1);
            if original.contains("next: ok") {
                assert!(view.text.ends_with("next: ok\n"));
            }
            if original.contains("label: ok") {
                assert!(view.text.ends_with("    label: ok\n"));
            }
        }
        let ordinary = "description: |\n  arbitrary multi word text\nnext: ok\n";
        assert_eq!(redact_context(ordinary).unwrap().text, ordinary);
    }

    #[test]
    fn valid_marker_only_exempts_its_own_bytes_inside_a_sensitive_value() {
        for input in [
            r#"password="<REDACTED_SECRET:API_KEY_1> exposed arbitrary phrase""#,
            "password: '<REDACTED_SECRET:API_KEY_1> exposed arbitrary phrase'",
            "password: |\n  <REDACTED_SECRET:API_KEY_1> exposed arbitrary phrase\nnext: ok\n",
        ] {
            let view = redact_context(input).unwrap();
            assert_eq!(view.spans.len(), 1);
            assert!(!view.text.contains("exposed arbitrary phrase"));
            assert_eq!(redact_context(&view.text).unwrap().text, view.text);
        }
    }

    #[test]
    fn each_detector_keeps_neighboring_public_identifiers_and_short_examples() {
        let cases = [
            (
                "credential_path",
                "/home/fixture/.ssh/id_ed25519",
                vec![
                    "/home/fixture/.ssh/id_ed25519.pub",
                    r"C:\Users\fixture\.ssh\id_rsa.pub",
                    "/repo/credentials.example.toml",
                ],
            ),
            (
                "credential_url",
                "postgres://user:synthetic-password@example.test/db",
                vec![
                    "https://example.test/docs",
                    "ssh://git@example.test/repo",
                    "postgres://user:@example.test/db",
                ],
            ),
            (
                "known_api_token",
                "ghp_syntheticabcdefghijklmnop",
                vec![
                    "ghp_example",
                    "github_pat_help",
                    "glpat-abcdefghijk",
                    "12345678-1234-1234-1234-123456789abc",
                ],
            ),
            (
                "jwt_token",
                "eyJhbGciOiJub25lIn0.eyJzdWIiOiJmaXh0dXJlIn0.c3ludGhldGljLXNpZ25hdHVyZQ",
                vec!["eyJ starts base64 JSON", "eyJabc.short.value", "v1.2.3"],
            ),
            (
                "bearer_token",
                "Bearer synthetic-credential-value",
                vec!["Bearer", "Bearer example", "Bearer abcdefg"],
            ),
            (
                "private_key",
                "-----BEGIN PRIVATE KEY-----\nsynthetic-private-body\n-----END PRIVATE KEY-----",
                vec![
                    "-----BEGIN PUBLIC KEY-----\npublic\n-----END PUBLIC KEY-----",
                    "-----BEGIN CERTIFICATE-----\npublic\n-----END CERTIFICATE-----",
                ],
            ),
        ];
        for (detector, positive, negatives) in cases {
            let spans = detect_secret_spans(positive).unwrap();
            assert_eq!(spans.len(), 1, "positive {detector}");
            assert_eq!(spans[0].detector_id, detector);
            for negative in negatives {
                assert_eq!(
                    redact_context(negative).unwrap().text,
                    negative,
                    "negative {detector}"
                );
            }
        }
    }
    #[test]
    fn original_spans_overlap_once_and_projection_is_idempotent() {
        let input = "OPENAI_API_KEY=sk-proj-syntheticabcdefghijklmnop\r\n";
        let result = redact_context(input).unwrap();
        assert_eq!(result.spans.len(), 1);
        assert_eq!(
            &input[result.spans[0].start_byte..result.spans[0].end_byte],
            "sk-proj-syntheticabcdefghijklmnop"
        );
        assert!(result.text.ends_with("\r\n"));
        assert_eq!(redact_context(&result.text).unwrap().text, result.text);
        assert!(redact_context(&result.text).unwrap().spans.is_empty());
    }
    #[test]
    fn utf8_multiline_and_partial_anchors_never_release_secret_bytes() {
        let input = "ação\r\n-----BEGIN OPENSSH PRIVATE KEY-----\r\nsynthetic-private-body\r\n-----END OPENSSH PRIVATE KEY-----\r\nafter";
        let result = redact_context(input).unwrap();
        assert_eq!(
            result.text.matches('\n').count(),
            input.matches('\n').count()
        );
        let split = input.find("private-body").unwrap();
        for (start, end) in [(0, split), (split, input.len())] {
            let text = render_redacted_range(input, &result.spans, start, end).unwrap();
            assert!(!text.contains("synthetic"));
            assert!(!text.contains("private-body"));
        }
        assert!(
            !redact_context("-----BEGIN PRIVATE KEY-----\nno-end")
                .unwrap()
                .text
                .contains("no-end")
        );
    }
    #[test]
    fn catalogue_positives_and_public_identifiers_are_distinguished() {
        for input in [
            "Bearer syntheticabcdefghijklmnop",
            "github_pat_syntheticabcdefghijklmnop",
            "glpat-syntheticabcdefghijklmnop",
            "AKIAABCDEFGHIJKLMNOP",
            "ASIAABCDEFGHIJKLMNOP",
            "sk_live_syntheticabcdefghijklmnop",
            "SG.syntheticabcdefghijklmnop.syntheticabcdefghijklmnopqrstuvwxyz12345",
            "password: synthetic-sensitive-value",
            "{\"TWILIO_AUTH_TOKEN\":\"synthetic-sensitive-value\"}",
            "redis://user:synthetic-pass@example.test",
        ] {
            assert!(
                !redact_context(input).unwrap().spans.is_empty(),
                "missing category"
            );
        }
        for input in [
            "12345678-1234-1234-1234-123456789abc",
            "SK0123456789abcdef0123456789abcdef",
            "-----BEGIN CERTIFICATE-----\npublic\n-----END CERTIFICATE-----",
            "token = <REDACTED_SECRET:API_KEY_1>",
            r#"password = """#,
            "token: ''",
        ] {
            assert_eq!(redact_context(input).unwrap().text, input);
        }
    }
    #[test]
    fn jwt_shaped_tokens_are_redacted() {
        for jwt in [
            "eyJhbGciOiJub25lIn0.eyJzdWIiOiJmaXh0dXJlIn0.c3ludGhldGljLXNpZ25hdHVyZQ",
            "eyJhbGciOiJub25lIn0.eyJzdWIiOiJmaXh0dXJlIn0.",
        ] {
            let result = redact_context(jwt).unwrap();
            assert_eq!(result.spans.len(), 1);
            assert!(!result.text.contains("eyJzdWIi"));
        }
    }
    #[test]
    fn authorization_is_local_to_exact_field_range() {
        let secret = "ghp_syntheticabcdefghijklmnop";
        let user = format!("{secret}\n{secret}");
        let prepared = PreparedModelInput::with_authorized_ranges(
            "",
            &user,
            "policy",
            &[],
            std::slice::from_ref(&(0..secret.len())),
        )
        .unwrap();
        assert_eq!(prepared.user_prompt().matches(secret).count(), 1);
        assert!(prepared.verify_matches("", prepared.user_prompt()).is_ok());
        assert!(prepared.verify_matches("", &user).is_err());
        assert!(!format!("{prepared:?}").contains(secret));
    }
    #[test]
    fn contextual_quoted_secrets_include_whitespace_and_escaped_quotes() {
        for input in [
            r#"password = "synthetic multi word password""#,
            r#"token: 'synthetic multi word token'"#,
            r#"{"client_secret":"synthetic \"quoted\" secret"}"#,
        ] {
            let result = redact_context(input).unwrap();
            assert_eq!(result.spans.len(), 1);
            assert!(!result.text.contains("synthetic"));
            assert!(!result.text.contains("quoted"));
            assert!(!result.text.contains("multi word"));
        }
    }
    #[test]
    fn untrusted_marker_text_cannot_hide_a_credential() {
        let original = "<REDACTED_SECRET:AKIAABCDEFGHIJKLMNOP>";
        let projected = redact_context(original).unwrap();
        assert_eq!(projected.spans.len(), 1);
        assert!(!projected.text.contains("AKIAABCDEFGHIJKLMNOP"));
    }
    #[test]
    fn dense_markers_do_not_exempt_neighboring_credentials() {
        let original =
            "<REDACTED_SECRET:KNOWN_API_TOKEN_1> ghp_syntheticabcdefghijklmnop ".repeat(4096);
        let result = redact_context(&original).unwrap();
        assert_eq!(result.spans.len(), 4096);
        assert!(!result.text.contains("ghp_synthetic"));
    }
    #[test]
    fn size_and_region_limits_fail_closed() {
        assert!(redact_secrets(&"x".repeat(MAX_SANITIZATION_BYTES + 1)).is_err());
        assert!(
            redact_context(&"ghp_syntheticabcdefghijklmnop ".repeat(MAX_REDACTION_SPANS + 1))
                .is_err()
        );
    }
}
