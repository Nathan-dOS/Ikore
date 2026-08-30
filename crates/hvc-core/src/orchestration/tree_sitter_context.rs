//! Versioned structural extraction over immutable source snapshots.

use std::collections::HashSet;
use std::time::Instant;

use tokio_util::sync::CancellationToken;
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

use super::*;

pub const TREE_SITTER_PARSER_VERSION: &str = "tree-sitter-0.26.13;rust-0.24.2;typescript-0.23.2";
pub const RUST_QUERY_VERSION: &str = "rust-structure-v1";
pub const TYPESCRIPT_QUERY_VERSION: &str = "typescript-structure-v1";
pub const LEXICAL_FALLBACK_VERSION: &str = "lexical-fallback-v1";

const RUST_QUERY: &str = r#"
(function_item name: (identifier) @name) @symbol.function
(struct_item name: (type_identifier) @name) @symbol.struct
(enum_item name: (type_identifier) @name) @symbol.enum
(trait_item name: (type_identifier) @name) @symbol.trait
(type_item name: (type_identifier) @name) @symbol.type_alias
(use_declaration) @reference.import
(call_expression function: (_) @reference.call)
"#;

const TYPESCRIPT_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @symbol.function
(method_definition name: [(property_identifier) (private_property_identifier)] @name) @symbol.method
(class_declaration name: (type_identifier) @name) @symbol.class
(abstract_class_declaration name: (type_identifier) @name) @symbol.class
(interface_declaration name: (type_identifier) @name) @symbol.interface
(type_alias_declaration name: (type_identifier) @name) @symbol.type_alias
(enum_declaration name: (identifier) @name) @symbol.enum
(import_statement) @reference.import
(call_expression function: (_) @reference.call)
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceLanguage {
    Rust,
    TypeScript,
    Tsx,
    Unsupported,
}

impl SourceLanguage {
    pub fn detect(path: &RelativeSourcePath) -> Self {
        match std::path::Path::new(path.as_str())
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("rs") => Self::Rust,
            Some("ts") => Self::TypeScript,
            Some("tsx") => Self::Tsx,
            _ => Self::Unsupported,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::Unsupported => "unsupported",
        }
    }

    pub fn query_version(self) -> &'static str {
        match self {
            Self::Rust => RUST_QUERY_VERSION,
            Self::TypeScript | Self::Tsx => TYPESCRIPT_QUERY_VERSION,
            Self::Unsupported => LEXICAL_FALLBACK_VERSION,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralParseStatus {
    Mapped,
    Partial,
    Unsupported,
}

impl StructuralParseStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mapped => "mapped",
            Self::Partial => "partial",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SourceStructuralFacts {
    pub source: SourceRevision,
    pub language: SourceLanguage,
    pub parser_version: &'static str,
    pub query_version: &'static str,
    pub status: StructuralParseStatus,
    pub symbols: Vec<StructuredSymbol>,
    pub references: Vec<ObservedReference>,
    pub diagnostics: Vec<String>,
    pub parse_elapsed_us: u64,
}

#[derive(Default)]
pub struct TreeSitterExtractor;

impl TreeSitterExtractor {
    pub fn extractor_revision() -> ExtractorRevision {
        ExtractorRevision {
            name: "tree-sitter-router".into(),
            version: TREE_SITTER_PARSER_VERSION.into(),
            policy_version: format!("{RUST_QUERY_VERSION};{TYPESCRIPT_QUERY_VERSION}"),
            resolution_context: None,
        }
    }

    pub fn extract_source(
        &self,
        source: &SourceSnapshot,
        cancellation: &CancellationToken,
    ) -> Result<SourceStructuralFacts, ContractError> {
        if cancellation.is_cancelled() {
            return Err(ContractError::Cancelled);
        }
        let started = Instant::now();
        let language = SourceLanguage::detect(&source.revision().relative_path);
        let mut facts = match language {
            SourceLanguage::Rust => self.parse_source(
                source,
                language,
                tree_sitter_rust::LANGUAGE.into(),
                RUST_QUERY,
            )?,
            SourceLanguage::TypeScript => self.parse_source(
                source,
                language,
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                TYPESCRIPT_QUERY,
            )?,
            SourceLanguage::Tsx => self.parse_source(
                source,
                language,
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                TYPESCRIPT_QUERY,
            )?,
            SourceLanguage::Unsupported => lexical_fallback(source),
        };
        facts.parse_elapsed_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        Ok(facts)
    }

    fn parse_source(
        &self,
        source: &SourceSnapshot,
        source_language: SourceLanguage,
        language: Language,
        query_source: &str,
    ) -> Result<SourceStructuralFacts, ContractError> {
        let mut parser = Parser::new();
        parser
            .set_language(&language)
            .map_err(|_| ContractError::Invalid("gramática Tree-sitter incompatível"))?;
        let tree = parser
            .parse(source.content(), None)
            .ok_or(ContractError::Invalid("parser Tree-sitter interrompido"))?;
        let query = Query::new(&language, query_source)
            .map_err(|_| ContractError::Invalid("query Tree-sitter inválida"))?;
        let capture_names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source.content().as_bytes());
        let mut symbols = Vec::new();
        let mut references = Vec::new();
        let mut seen_symbols = HashSet::new();
        let mut seen_references = HashSet::new();

        while let Some(query_match) = matches.next() {
            let name_capture = query_match
                .captures
                .iter()
                .find(|capture| capture_names[capture.index as usize] == "name");
            for capture in query_match.captures {
                let capture_name = capture_names[capture.index as usize];
                if let Some(kind) = symbol_kind(capture_name, capture.node) {
                    let Some(name_capture) = name_capture else {
                        continue;
                    };
                    let name = node_text(source.content(), name_capture.node);
                    let start_line = capture.node.start_position().row as u32 + 1;
                    let end_line = capture.node.end_position().row as u32 + 1;
                    if seen_symbols.insert((kind, name.clone(), start_line, end_line)) {
                        symbols.push(StructuredSymbol {
                            source: source.revision().clone(),
                            kind,
                            name,
                            start_line,
                            end_line,
                            signature: declaration_signature(source.content(), capture.node),
                        });
                    }
                } else if let Some(relation) = reference_relation(capture_name) {
                    let observed_text = node_text(source.content(), capture.node);
                    let line = capture.node.start_position().row as u32 + 1;
                    if !observed_text.trim().is_empty()
                        && seen_references.insert((relation, observed_text.clone(), line))
                    {
                        references.push(ObservedReference {
                            source: source.revision().clone(),
                            line,
                            observed_text,
                            relation,
                            resolution: ReferenceResolution::Unresolved {
                                reason: "Aresta estrutural observada; resolvedores de linguagem ficam fora da BF2."
                                    .into(),
                            },
                        });
                    }
                }
            }
        }

        symbols.sort_by_key(|symbol| (symbol.start_line, symbol.end_line, symbol.name.clone()));
        references.sort_by_key(|reference| (reference.line, reference.observed_text.clone()));
        let has_error = tree.root_node().has_error();
        Ok(SourceStructuralFacts {
            source: source.revision().clone(),
            language: source_language,
            parser_version: TREE_SITTER_PARSER_VERSION,
            query_version: source_language.query_version(),
            status: if has_error {
                StructuralParseStatus::Partial
            } else {
                StructuralParseStatus::Mapped
            },
            symbols,
            references,
            diagnostics: if has_error {
                vec!["syntax_error: árvore parcial preservada; fatos ausentes não são evidência negativa."
                    .into()]
            } else {
                Vec::new()
            },
            parse_elapsed_us: 0,
        })
    }
}

impl CodeContextExtractor for TreeSitterExtractor {
    fn extract(
        &self,
        sources: &[SourceSnapshot],
        budget: CollectionBudget,
        cancellation: &CancellationToken,
    ) -> Result<CodeContextFacts, ContractError> {
        budget.validate()?;
        let started = Instant::now();
        let project_id = sources
            .first()
            .map(|source| source.revision().project_id.clone())
            .unwrap_or_else(|| "empty-structural-scope".into());
        if sources
            .iter()
            .any(|source| source.revision().project_id != project_id)
        {
            return Err(ContractError::Invalid("fontes de projetos distintos"));
        }
        let mut facts = CodeContextFacts {
            schema_version: SchemaV1,
            project_id,
            extractor: Self::extractor_revision(),
            scope: "bounded-structural-extraction".into(),
            candidate_set_sha256: Some(ContentSha256::of(
                &serde_json::to_vec(
                    &sources
                        .iter()
                        .map(SourceSnapshot::revision)
                        .collect::<Vec<_>>(),
                )
                .expect("source revisions"),
            )),
            sources: Vec::new(),
            snippets: Vec::new(),
            symbols: Vec::new(),
            references: Vec::new(),
            coverage: ContextCoverage {
                state: CoverageState::Complete,
                documentation: DocumentationAvailability::Unknown,
                enumerated_files: 0,
                read_bytes: 0,
                truncated: false,
                diagnostics: Vec::new(),
            },
        };

        for source in sources {
            if cancellation.is_cancelled() {
                return Err(ContractError::Cancelled);
            }
            let source_bytes = source.content().len() as u64;
            if facts.coverage.enumerated_files >= budget.max_files
                || facts.coverage.read_bytes.saturating_add(source_bytes) > budget.max_read_bytes
                || started.elapsed().as_millis() >= u128::from(budget.max_duration_ms)
            {
                facts.coverage.truncated = true;
                facts.coverage.state = CoverageState::Partial;
                facts.coverage.diagnostics.push(
                    "budget_exhausted: extração estrutural devolveu cobertura parcial.".into(),
                );
                break;
            }
            let source_facts = self.extract_source(source, cancellation)?;
            facts.coverage.enumerated_files += 1;
            facts.coverage.read_bytes += source_bytes;
            facts.sources.push(source_facts.source);
            facts.symbols.extend(source_facts.symbols);
            facts.references.extend(source_facts.references);
            if source_facts.status == StructuralParseStatus::Partial {
                facts.coverage.state = CoverageState::Partial;
            }
            if source_facts.status == StructuralParseStatus::Unsupported {
                facts.coverage.diagnostics.push(format!(
                    "lexical_fallback: `{}` não possui gramática BF2.",
                    source.revision().relative_path.as_str()
                ));
            }
            facts.coverage.diagnostics.extend(source_facts.diagnostics);
        }
        facts.validate_against(sources)?;
        Ok(facts)
    }
}

fn symbol_kind(capture_name: &str, node: tree_sitter::Node<'_>) -> Option<StructuredSymbolKind> {
    Some(match capture_name {
        "symbol.function" if is_method_node(node) => StructuredSymbolKind::Method,
        "symbol.function" => StructuredSymbolKind::Function,
        "symbol.method" => StructuredSymbolKind::Method,
        "symbol.struct" => StructuredSymbolKind::Struct,
        "symbol.class" => StructuredSymbolKind::Class,
        "symbol.trait" => StructuredSymbolKind::Trait,
        "symbol.interface" => StructuredSymbolKind::Interface,
        "symbol.enum" => StructuredSymbolKind::Enum,
        "symbol.type_alias" => StructuredSymbolKind::TypeAlias,
        _ => return None,
    })
}

fn is_method_node(mut node: tree_sitter::Node<'_>) -> bool {
    while let Some(parent) = node.parent() {
        if matches!(parent.kind(), "impl_item" | "trait_item") {
            return true;
        }
        node = parent;
    }
    false
}

fn reference_relation(capture_name: &str) -> Option<ReferenceRelation> {
    match capture_name {
        "reference.import" => Some(ReferenceRelation::Import),
        "reference.call" => Some(ReferenceRelation::Call),
        _ => None,
    }
}

fn node_text(content: &str, node: tree_sitter::Node<'_>) -> String {
    content
        .get(node.byte_range())
        .unwrap_or_default()
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(512)
        .collect()
}

fn declaration_signature(content: &str, node: tree_sitter::Node<'_>) -> String {
    let declaration = content.get(node.byte_range()).unwrap_or_default();
    declaration
        .split_once('{')
        .map_or(declaration, |(header, _)| header)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(512)
        .collect()
}

fn lexical_fallback(source: &SourceSnapshot) -> SourceStructuralFacts {
    let mut references = Vec::new();
    for (index, line) in source.content().lines().enumerate() {
        let observed = line.trim();
        if ["import ", "from ", "use ", "#include ", "mod "]
            .iter()
            .any(|prefix| observed.starts_with(prefix))
            || observed.contains("require(")
        {
            references.push(ObservedReference {
                source: source.revision().clone(),
                line: index as u32 + 1,
                observed_text: observed.chars().take(512).collect(),
                relation: ReferenceRelation::Lexical,
                resolution: ReferenceResolution::Unresolved {
                    reason: "Fallback lexical; linguagem sem gramática BF2.".into(),
                },
            });
        }
    }
    SourceStructuralFacts {
        source: source.revision().clone(),
        language: SourceLanguage::Unsupported,
        parser_version: TREE_SITTER_PARSER_VERSION,
        query_version: LEXICAL_FALLBACK_VERSION,
        status: StructuralParseStatus::Unsupported,
        symbols: Vec::new(),
        references,
        diagnostics: Vec::new(),
        parse_elapsed_us: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(path: &str, content: &str) -> SourceSnapshot {
        SourceSnapshot::from_text(
            "project-a",
            RelativeSourcePath::new(path).unwrap(),
            content.into(),
        )
        .unwrap()
    }

    #[test]
    fn rust_and_typescript_emit_versioned_symbols_and_edges() {
        let extractor = TreeSitterExtractor;
        let rust = source(
            "src/lib.rs",
            "use crate::auth::Token;\nstruct Session { token: Token }\nfn validate() { verify(); }\n",
        );
        let typescript = source(
            "web/auth.ts",
            "import { api } from './api';\ninterface Token { value: string }\nclass Session { run() { api(); } }\n",
        );
        let facts = extractor
            .extract(
                &[rust.clone(), typescript.clone()],
                CollectionBudget::default(),
                &CancellationToken::new(),
            )
            .unwrap();
        assert!(facts.symbols.iter().any(|symbol| {
            symbol.kind == StructuredSymbolKind::Struct && symbol.name == "Session"
        }));
        assert!(facts.symbols.iter().any(|symbol| {
            symbol.kind == StructuredSymbolKind::Interface && symbol.name == "Token"
        }));
        assert!(
            facts
                .references
                .iter()
                .any(|reference| reference.relation == ReferenceRelation::Import)
        );
        assert!(
            facts
                .references
                .iter()
                .any(|reference| reference.relation == ReferenceRelation::Call)
        );
        assert_eq!(facts.extractor.version, TREE_SITTER_PARSER_VERSION);
        facts.validate_against(&[rust, typescript]).unwrap();
    }

    #[test]
    fn unsupported_language_uses_lexical_fallback() {
        let source = source("src/app.py", "from auth import token\nverify(token)\n");
        let facts = TreeSitterExtractor
            .extract(
                std::slice::from_ref(&source),
                CollectionBudget::default(),
                &CancellationToken::new(),
            )
            .unwrap();
        assert!(facts.symbols.is_empty());
        assert_eq!(facts.references[0].relation, ReferenceRelation::Lexical);
        assert!(facts.coverage.diagnostics[0].starts_with("lexical_fallback:"));
    }

    #[test]
    fn syntax_error_is_partial_instead_of_catastrophic() {
        let source = source("src/lib.rs", "struct Session {\nfn validate( { verify();\n");
        let facts = TreeSitterExtractor
            .extract(
                std::slice::from_ref(&source),
                CollectionBudget::default(),
                &CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(facts.coverage.state, CoverageState::Partial);
        assert!(
            facts
                .coverage
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.starts_with("syntax_error:"))
        );
    }

    #[test]
    fn body_only_change_changes_the_source_revision() {
        let before = source("src/lib.rs", "fn value() -> u8 { 1 }\n");
        let after = source("src/lib.rs", "fn value() -> u8 { 2 }\n");
        assert_ne!(
            before.revision().content_sha256,
            after.revision().content_sha256
        );
    }

    #[test]
    fn cancellation_and_budget_are_cooperative() {
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let source = source("src/lib.rs", "fn value() {}\n");
        assert_eq!(
            TreeSitterExtractor.extract(
                std::slice::from_ref(&source),
                CollectionBudget::default(),
                &cancelled,
            ),
            Err(ContractError::Cancelled)
        );

        let budget = CollectionBudget {
            max_read_bytes: 1,
            max_file_bytes: 1,
            ..CollectionBudget::default()
        };
        let facts = TreeSitterExtractor
            .extract(&[source], budget, &CancellationToken::new())
            .unwrap();
        assert_eq!(facts.coverage.state, CoverageState::Partial);
        assert!(facts.coverage.truncated);
    }
}
