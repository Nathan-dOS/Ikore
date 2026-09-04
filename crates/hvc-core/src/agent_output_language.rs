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
    validate_english_text("artifact name", name)?;
    if let Some(relative_path) = relative_path {
        validate_english_text("artifact path", relative_path)?;
    }
    validate_english_text("artifact content", content)
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
    fn exposes_the_canonical_language_contract() {
        assert_eq!(AGENT_OUTPUT_LANGUAGE, "en");
        assert!(ENGLISH_ONLY_INSTRUCTION.contains("MUST be written in English"));
    }
}
