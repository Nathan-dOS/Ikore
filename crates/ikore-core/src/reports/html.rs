//! Escaped HTML building blocks shared by host-rendered reports.
//!
//! Every runtime string reaches the output through [`SafeHtml::text`],
//! [`SafeHtml::push_text`] or [`SafeHtml::push_attr`], which sanitize and escape it. Markup
//! only comes from `&'static str` literals written in this crate, so text that
//! came from a model or a repository can never become a tag or an attribute.

use std::fmt;

/// Content-Security-Policy of every report: no script, no network, no frames,
/// no form submission. Styles are inline and images may only be `data:` URIs.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'";

const STYLESHEET: &str = include_str!("report.css");

/// HTML that is safe to emit: escaped runtime text plus host-authored literals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SafeHtml(String);

impl SafeHtml {
    pub fn new() -> Self {
        Self::default()
    }

    /// Escapes untrusted text.
    pub fn text(value: &str) -> Self {
        let mut html = Self::new();
        html.push_text(value);
        html
    }

    /// Host-authored markup. Only literals qualify; runtime data never does.
    pub fn markup(value: &'static str) -> Self {
        Self(value.to_owned())
    }

    pub fn push(&mut self, other: &SafeHtml) -> &mut Self {
        self.0.push_str(&other.0);
        self
    }

    pub fn push_markup(&mut self, value: &'static str) -> &mut Self {
        self.0.push_str(value);
        self
    }

    pub fn push_text(&mut self, value: &str) -> &mut Self {
        escape_into(
            &mut self.0,
            &crate::sanitization::sanitized_observation(value),
        );
        self
    }

    /// Appends ` name="value"` with the value escaped. Call it right after an
    /// opening tag literal that has not been closed yet.
    pub fn push_attr(&mut self, name: &'static str, value: &str) -> &mut Self {
        self.0.push(' ');
        self.0.push_str(name);
        self.0.push_str("=\"");
        escape_into(
            &mut self.0,
            &crate::sanitization::sanitized_observation(value),
        );
        self.0.push('"');
        self
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for SafeHtml {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Escapes the five characters that can change HTML structure in text and in
/// quoted attribute values.
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    escape_into(&mut out, value);
    out
}

fn escape_into(out: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
}

/// `<tag class="class">body</tag>`; tag and class are host literals.
pub fn element(tag: &'static str, class: &'static str, body: &SafeHtml) -> SafeHtml {
    let mut html = SafeHtml::new();
    html.push_markup("<").push_markup(tag);
    if !class.is_empty() {
        html.push_markup(" class=\"")
            .push_markup(class)
            .push_markup("\"");
    }
    html.push_markup(">")
        .push(body)
        .push_markup("</")
        .push_markup(tag)
        .push_markup(">");
    html
}

/// A complete, self-contained document: charset, viewport, the report CSP,
/// the shared stylesheet and `body`. Nothing in it references another file.
pub fn document(title: &str, description: &str, body: &SafeHtml) -> String {
    let mut html = SafeHtml::markup(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<meta http-equiv=\"Content-Security-Policy\"",
    );
    html.push_attr("content", CONTENT_SECURITY_POLICY)
        .push_markup(">\n<meta name=\"referrer\" content=\"no-referrer\">\n<meta name=\"color-scheme\" content=\"light dark\">\n<meta name=\"description\"")
        .push_attr("content", description)
        .push_markup(">\n<title>")
        .push_text(title)
        .push_markup("</title>\n<style>\n")
        .push_markup(STYLESHEET)
        .push_markup("</style>\n</head>\n<body>\n")
        .push(body)
        .push_markup("\n</body>\n</html>\n");
    html.into_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_text_and_attributes_are_sanitized_before_html_escaping() {
        let encoded = r#"{"message":"sk-proj-\u0061bcdefghijklmnopqrst"}"#;
        let mut html = SafeHtml::markup("<span");
        html.push_attr("title", encoded)
            .push_markup(">")
            .push_text(encoded)
            .push_markup("</span>");
        let output = html.into_string();
        assert!(!output.contains("bcdefghijklmnopqrst"));
        assert!(!output.contains("\\u0061"));
        assert!(output.contains("&lt;REDACTED_SECRET:"));
        assert!(!output.contains("<REDACTED_SECRET:"));
        // HTML escaping remains a separate public primitive.
        assert_eq!(
            escape("sk-proj-syntheticabcdefghijklmnop"),
            "sk-proj-syntheticabcdefghijklmnop"
        );
    }

    #[test]
    fn escapes_every_structural_character() {
        assert_eq!(
            escape(r#"<a href="x" onclick='y'>&</a>"#),
            "&lt;a href=&quot;x&quot; onclick=&#39;y&#39;&gt;&amp;&lt;/a&gt;"
        );
        assert_eq!(escape("ção → ok"), "ção → ok");
    }

    #[test]
    fn attributes_and_text_cannot_open_markup() {
        let mut html = SafeHtml::markup("<span");
        html.push_attr("title", "\"><script>alert(1)</script>")
            .push_markup(">")
            .push_text("</span><img src=x onerror=alert(1)>")
            .push_markup("</span>");
        let out = html.into_string();
        assert!(!out.contains("<script"));
        assert!(!out.contains("<img"));
        assert!(out.starts_with("<span title=\"&quot;&gt;&lt;script&gt;"));
    }

    #[test]
    fn document_carries_the_policy_and_no_external_reference() {
        let out = document("T <1>", "D \"2\"", &SafeHtml::text("body"));
        assert!(out.contains(
            "http-equiv=\"Content-Security-Policy\" content=\"default-src &#39;none&#39;;"
        ));
        assert!(out.contains("<title>T &lt;1&gt;</title>"));
        assert!(out.contains("content=\"D &quot;2&quot;\""));
        for forbidden in ["<script", "<link", "@import", "http://", "https://", "src="] {
            assert!(!out.contains(forbidden), "document contains {forbidden}");
        }
    }
}
