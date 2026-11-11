//! Fail-closed language policy for agent-authored artifacts.

use std::collections::HashSet;

use thiserror::Error;

pub const AGENT_OUTPUT_LANGUAGE: &str = "en";

pub const ENGLISH_ONLY_INSTRUCTION: &str = "All agent-authored output MUST be written in English. This includes JSON string values, artifact names, generated filenames, Markdown, documentation, code comments, test descriptions, reports, and user-facing text stored in artifacts. Preserve opaque identifiers, hashes, API names, and existing source paths exactly; never translate them.";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("agent-authored {field} must be English; detected non-English marker `{marker}`")]
pub struct AgentOutputLanguageError {
    field: &'static str,
    marker: String,
}

impl AgentOutputLanguageError {
    fn new(field: &'static str, marker: impl Into<String>) -> Self {
        Self {
            field,
            marker: marker.into(),
        }
    }
}

/// Validates natural-language text with a deliberately conservative, deterministic guard.
///
/// The prompt contract is the primary language control. This guard is the publication
/// boundary: it rejects non-ASCII alphabetic scripts and high-signal non-English vocabulary,
/// including ASCII-only Portuguese that would otherwise bypass an accent check.
pub fn validate_english_text(
    field: &'static str,
    value: &str,
) -> Result<(), AgentOutputLanguageError> {
    validate_span(field, value)
}

/// Same guard, applied only to the parts of a document that are prose.
///
/// Use this for text the model composed and the strict form for the short
/// identifiers it invented - an artifact name or a destination path is a whole
/// token the model chose, and it must be English.
pub fn validate_english_prose(
    field: &'static str,
    value: &str,
) -> Result<(), AgentOutputLanguageError> {
    validate_span(field, &prose_only(value))
}

fn validate_span(field: &'static str, value: &str) -> Result<(), AgentOutputLanguageError> {
    if let Some(character) = value
        .chars()
        .find(|character| character.is_alphabetic() && !character.is_ascii())
    {
        return Err(AgentOutputLanguageError::new(field, character.to_string()));
    }

    let words = ascii_words(value);
    if let Some(word) = words
        .iter()
        .find(|word| STRONG_NON_ENGLISH_MARKERS.contains(&word.as_str()))
    {
        return Err(AgentOutputLanguageError::new(field, word.clone()));
    }

    let weak_markers = words
        .iter()
        .filter(|word| WEAK_NON_ENGLISH_MARKERS.contains(&word.as_str()))
        .collect::<HashSet<_>>();
    if weak_markers.len() >= 2 {
        let mut markers = weak_markers
            .into_iter()
            .map(|value| value.as_str())
            .collect::<Vec<_>>();
        markers.sort_unstable();
        return Err(AgentOutputLanguageError::new(field, markers.join("+")));
    }

    Ok(())
}

pub fn validate_agent_artifact(
    name: &str,
    relative_path: Option<&str>,
    content: &str,
) -> Result<(), AgentOutputLanguageError> {
    // The name and the destination path are identifiers the model invented, so
    // they are judged whole. The body is prose that legitimately quotes paths
    // and identifiers it was required to copy verbatim.
    validate_english_text("artifact name", name)?;
    if let Some(relative_path) = relative_path {
        validate_english_text("artifact path", relative_path)?;
    }
    validate_english_prose("artifact content", content)
}

/// Drops the spans this guard has no business judging: code spans and fenced
/// blocks, and any token that is a path, URL or filename.
///
/// The contract says opaque identifiers and existing source paths must be
/// preserved exactly, never translated - so the guard cannot then reject the
/// document for containing them. In a workspace whose own files and task titles
/// are Portuguese, a correct English report naturally quotes
/// `knowledge/obsidian/tarefas/relatorio.md`, and judging that as prose failed
/// the whole report over text the model was required to copy verbatim.
///
/// Deliberately narrow: everything outside those spans is still judged in full,
/// so genuine non-English prose is caught exactly as before.
fn prose_only(value: &str) -> String {
    let mut prose = String::with_capacity(value.len());
    let mut in_code_span = false;
    for token in value.split_inclusive(char::is_whitespace) {
        let backticks = token.matches('`').count();
        let opened_here = in_code_span;
        if backticks % 2 == 1 {
            in_code_span = !in_code_span;
        }
        if opened_here || backticks > 0 || is_path_like(token.trim()) {
            // Keep the whitespace so word boundaries elsewhere are unchanged.
            prose.extend(token.chars().filter(|character| character.is_whitespace()));
            continue;
        }
        prose.push_str(token);
    }
    prose
}

/// A token that names a file, path or URL rather than saying something.
fn is_path_like(token: &str) -> bool {
    let token = token.trim_matches(|character: char| !character.is_alphanumeric());
    if token.is_empty() {
        return false;
    }
    token.contains('/')
        || token.contains('\\')
        || token.contains("://")
        || token.contains('_')
        || is_opaque_identifier_or_hash(token)
        || matches!(
            token.rsplit_once('.'),
            Some((_, extension))
                if (1..=5).contains(&extension.len())
                    && extension.chars().all(|character| character.is_ascii_alphanumeric())
        )
}

fn is_opaque_identifier_or_hash(token: &str) -> bool {
    // Hex UUIDs (e.g. 550e8400-e29b-41d4-a716-446655440000) or hex hashes (>= 8 chars with digits)
    token.len() >= 8
        && token.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        && token.chars().any(|c| c.is_ascii_digit())
}

fn ascii_words(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        if character.is_ascii_alphabetic() {
            current.push(character.to_ascii_lowercase());
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

const STRONG_NON_ENGLISH_MARKERS: &[&str] = &[
    // Portuguese, including common unaccented model output.
    "agora",
    "ainda",
    "alteracao",
    "aplicacao",
    "aplicativo",
    "aprovado",
    "aprendizado",
    "arquivo",
    "arquivos",
    "autenticacao",
    "chamada",
    "codigo",
    "componente",
    "componentes",
    "conclusao",
    "contrato",
    "dados",
    "decisao",
    "etapa",
    "evidencia",
    "evidencias",
    "execucao",
    "gerado",
    "gerada",
    "implementacao",
    "mensagem",
    "mensagens",
    "nenhum",
    "nenhuma",
    "observacao",
    "observacoes",
    "pergunta",
    "perguntas",
    "projeto",
    "relatorio",
    "relatorios",
    "resultado",
    "resumo",
    "tarefa",
    "teste",
    "testes",
    "usuario",
    "validacao",
    "versao",
    // High-signal Spanish, French, German, and Italian words.
    "archivo",
    "prueba",
    "resumen",
    "fichier",
    "rapport",
    "datei",
    "ergebnis",
    "bericht",
    "senza",
    "risultato",
];

const WEAK_NON_ENGLISH_MARKERS: &[&str] = &[
    "aos", "como", "com", "da", "das", "de", "deve", "devem", "do", "dos", "esta", "este", "foi",
    "mais", "nao", "para", "pela", "pelo", "por", "que", "sem", "sua", "uma", "con", "del", "los",
    "sin", "una", "avec", "les", "sans", "une", "der", "die", "mit", "ohne", "und", "che",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_english_markdown_json_and_source_code() {
        for value in [
            "# Authentication contract\n\nThe token is stored in the operating-system keyring.",
            r#"{"summary":"The tests passed.","status":"complete"}"#,
            "pub fn validate_token() { // Reject an expired token.\n}",
        ] {
            validate_english_text("fixture", value).unwrap();
        }
    }

    #[test]
    fn rejects_accented_and_ascii_only_portuguese() {
        let accented = validate_english_text("fixture", "Relatório de validação").unwrap_err();
        assert!(accented.to_string().contains("must be English"));

        let ascii_only =
            validate_english_text("fixture", "Este arquivo foi gerado sem chamada externa")
                .unwrap_err();
        assert!(ascii_only.to_string().contains("arquivo"));
    }

    #[test]
    fn rejects_non_english_artifact_name_path_or_content() {
        assert!(
            validate_agent_artifact("Relatorio", Some("reports/output.md"), "English body")
                .is_err()
        );
        assert!(
            validate_agent_artifact("Report", Some("relatorios/output.md"), "English body")
                .is_err()
        );
        assert!(
            validate_agent_artifact("Report", Some("reports/output.md"), "Nenhum teste falhou")
                .is_err()
        );
    }

    #[test]
    fn prose_keeps_the_paths_and_identifiers_the_contract_forbids_translating() {
        // The contract requires opaque identifiers and existing source paths to
        // be preserved exactly. Judging them as prose failed correct English
        // reports over the very text the model was told to copy verbatim.
        validate_english_prose(
            "fixture",
            "The report cites knowledge/obsidian/tarefas/relatorio-de-execucao.md and the `resultado_final` field.",
        )
        .unwrap();
        validate_english_prose(
            "fixture",
            "Coverage is recorded in docs/arquitetura/contrato.md; see also https://example.com/relatorio.",
        )
        .unwrap();
        validate_english_prose(
            "fixture",
            "Feature id is 12345678-abcd-1234-de00-da0012345678 and sha is a06f8194b3c2.",
        )
        .unwrap();

        // Prose outside those spans is still judged in full.
        let portuguese = validate_english_prose(
            "fixture",
            "O arquivo docs/arquitetura/contrato.md foi gerado sem chamada externa",
        )
        .unwrap_err();
        assert!(portuguese.to_string().contains("arquivo"));
        assert!(
            validate_english_prose("fixture", "Relatório de validação da etapa").is_err(),
            "accented prose is still rejected"
        );
    }

    #[test]
    fn a_name_or_destination_path_the_model_invented_is_still_judged_whole() {
        // These are single tokens the model chose, not text it had to quote.
        assert!(
            validate_agent_artifact("Relatorio", Some("reports/output.md"), "English body")
                .is_err()
        );
        assert!(
            validate_agent_artifact("Report", Some("relatorios/output.md"), "English body")
                .is_err()
        );
        // ...while the body may quote that same path.
        validate_agent_artifact(
            "Report",
            Some("reports/output.md"),
            "Superseded by relatorios/output.md, which the workspace already owns.",
        )
        .unwrap();
    }

    #[test]
    fn exposes_the_canonical_language_contract() {
        assert_eq!(AGENT_OUTPUT_LANGUAGE, "en");
        assert!(ENGLISH_ONLY_INSTRUCTION.contains("MUST be written in English"));
    }
}
