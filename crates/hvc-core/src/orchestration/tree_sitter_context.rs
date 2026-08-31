//! Versioned structural extraction over immutable source snapshots.

use std::collections::HashSet;
use std::time::Instant;

use tokio_util::sync::CancellationToken;
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

use super::*;

pub const TREE_SITTER_PARSER_VERSION: &str = "tree-sitter-0.26.13;rust-0.24.2;typescript-0.23.2";
pub const RUST_QUERY_VERSION: &str = "rust-structure-v2";
pub const TYPESCRIPT_QUERY_VERSION: &str = "typescript-structure-v2";
pub const LEXICAL_FALLBACK_VERSION: &str = "lexical-fallback-v1";

const RUST_QUERY: &str = r#"
(function_item name: (identifier) @name) @symbol.function
(struct_item name: (type_identifier) @name) @symbol.struct
(enum_item name: (type_identifier) @name) @symbol.enum
(trait_item name: (type_identifier) @name) @symbol.trait
(type_item name: (type_identifier) @name) @symbol.type_alias
(use_declaration) @reference.import
(mod_item) @reference.import
(call_expression function: (_) @reference.call)
(impl_item trait: (_)) @reference.implements
(type_identifier) @reference.type_use
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
(export_statement) @reference.reexport
(call_expression function: (_) @reference.call)
(extends_clause (_) @reference.inherits)
(extends_type_clause (_) @reference.inherits)
(implements_clause (_) @reference.implements)
(type_identifier) @reference.type_use
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
                        let signature = declaration_signature(source.content(), capture.node);
                        let qualified_name = qualified_symbol_name(
                            source.revision().relative_path.as_str(),
                            capture.node,
                            &name,
                            source.content(),
                        );
                        let symbol_key = ContentSha256::of(
                            format!(
                                "{}:{kind:?}:{qualified_name}:{signature}",
                                source.revision().relative_path.as_str()
                            )
                            .as_bytes(),
                        )
                        .as_str()
                        .to_string();
                        symbols.push(StructuredSymbol {
                            source: source.revision().clone(),
                            kind,
                            name,
                            symbol_key: Some(symbol_key),
                            qualified_name: Some(qualified_name),
                            owner_symbol_key: None,
                            visibility: symbol_visibility(
                                source_language,
                                capture.node,
                                source.content(),
                            ),
                            start_line,
                            end_line,
                            signature,
                        });
                    }
                } else if let Some(relation) = reference_relation(capture_name) {
                    let Some(descriptor) = reference_descriptor(
                        source_language,
                        relation,
                        capture.node,
                        source.content(),
                    ) else {
                        continue;
                    };
                    let observed_text = node_text(source.content(), capture.node);
                    let line = capture.node.start_position().row as u32 + 1;
                    if !observed_text.trim().is_empty()
                        && seen_references.insert((
                            relation,
                            observed_text.clone(),
                            line,
                            capture.node.start_byte(),
                        ))
                    {
                        let descriptor_json =
                            serde_json::to_vec(&descriptor).expect("reference descriptor");
                        let reference_key = ContentSha256::of(
                            &[
                                source.revision().relative_path.as_str().as_bytes(),
                                source.revision().content_sha256.as_str().as_bytes(),
                                &capture.node.start_byte().to_le_bytes(),
                                &descriptor_json,
                            ]
                            .concat(),
                        )
                        .as_str()
                        .to_string();
                        references.push(ObservedReference {
                            source: source.revision().clone(),
                            line,
                            observed_text,
                            relation,
                            reference_key: Some(reference_key),
                            descriptor: Some(descriptor),
                            source_symbol_key: None,
                            target_symbol_key: None,
                            target_revision: None,
                            resolution_evidence: None,
                            resolution: ReferenceResolution::Unresolved {
                                reason: "Referência estrutural normalizada; resolução de projeto pendente."
                                    .into(),
                            },
                        });
                    }
                }
            }
        }

        symbols.sort_by_key(|symbol| (symbol.start_line, symbol.end_line, symbol.name.clone()));
        references.sort_by_key(|reference| (reference.line, reference.observed_text.clone()));
        let owner_keys = symbols
            .iter()
            .filter_map(|symbol| Some((symbol.qualified_name.clone()?, symbol.symbol_key.clone()?)))
            .collect::<std::collections::HashMap<_, _>>();
        for symbol in &mut symbols {
            if symbol.kind == StructuredSymbolKind::Method
                && let Some((owner, _)) = symbol
                    .qualified_name
                    .as_deref()
                    .and_then(|name| name.rsplit_once("::"))
            {
                symbol.owner_symbol_key = owner_keys.get(owner).cloned();
            }
        }
        for reference in &mut references {
            reference.source_symbol_key = symbols
                .iter()
                .filter(|symbol| {
                    symbol.start_line <= reference.line && reference.line <= symbol.end_line
                })
                .min_by_key(|symbol| symbol.end_line.saturating_sub(symbol.start_line))
                .and_then(|symbol| symbol.symbol_key.clone());
        }
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
        "reference.reexport" => Some(ReferenceRelation::Reexport),
        "reference.call" => Some(ReferenceRelation::Call),
        "reference.type_use" => Some(ReferenceRelation::TypeUse),
        "reference.inherits" => Some(ReferenceRelation::Inherits),
        "reference.implements" => Some(ReferenceRelation::Implements),
        _ => None,
    }
}

fn qualified_symbol_name(
    relative_path: &str,
    node: tree_sitter::Node<'_>,
    name: &str,
    content: &str,
) -> String {
    let module = relative_path
        .trim_start_matches("src/")
        .trim_end_matches("/mod.rs")
        .trim_end_matches(".rs")
        .trim_end_matches(".ts")
        .trim_end_matches(".tsx")
        .replace('/', "::");
    let mut owner = None;
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if matches!(
            parent.kind(),
            "class_declaration" | "abstract_class_declaration" | "interface_declaration"
        ) {
            owner = parent
                .child_by_field_name("name")
                .map(|child| node_text(content, child));
            break;
        }
        if parent.kind() == "impl_item" {
            owner = parent
                .child_by_field_name("type")
                .map(|child| node_text(content, child));
            break;
        }
        ancestor = parent.parent();
    }
    [
        (!module.is_empty()).then_some(module),
        owner.filter(|value| !value.is_empty()),
        Some(name.to_string()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("::")
}

fn symbol_visibility(
    language: SourceLanguage,
    node: tree_sitter::Node<'_>,
    content: &str,
) -> SymbolVisibility {
    match language {
        SourceLanguage::Rust => {
            let text = content
                .get(node.byte_range())
                .unwrap_or_default()
                .trim_start();
            if text.starts_with("pub ") || text.starts_with("pub(") {
                SymbolVisibility::Public
            } else {
                SymbolVisibility::Private
            }
        }
        SourceLanguage::TypeScript | SourceLanguage::Tsx => {
            let mut current = Some(node);
            while let Some(candidate) = current {
                if candidate.kind() == "export_statement" {
                    return SymbolVisibility::Exported;
                }
                current = candidate.parent();
            }
            SymbolVisibility::Private
        }
        SourceLanguage::Unsupported => SymbolVisibility::Unknown,
    }
}

fn reference_descriptor(
    language: SourceLanguage,
    relation: ReferenceRelation,
    node: tree_sitter::Node<'_>,
    content: &str,
) -> Option<ReferenceDescriptor> {
    let text = content.get(node.byte_range()).unwrap_or_default().trim();
    match relation {
        ReferenceRelation::Import | ReferenceRelation::Reexport => {
            if node.kind() == "export_statement" && node.child_by_field_name("source").is_none() {
                return None;
            }
            let specifier = match language {
                SourceLanguage::Rust if node.kind() == "mod_item" => node
                    .child_by_field_name("name")
                    .map(|child| node_text(content, child))?,
                SourceLanguage::Rust => text
                    .trim_start_matches("pub ")
                    .trim_start_matches("use ")
                    .trim_end_matches(';')
                    .trim()
                    .to_string(),
                SourceLanguage::TypeScript | SourceLanguage::Tsx => {
                    node.child_by_field_name("source").map(|child| {
                        node_text(content, child)
                            .trim_matches(['\'', '"'])
                            .to_string()
                    })?
                }
                SourceLanguage::Unsupported => return None,
            };
            Some(ReferenceDescriptor::Module {
                bindings: import_bindings(language, text),
                specifier,
            })
        }
        ReferenceRelation::Call => Some(ReferenceDescriptor::Symbol {
            path: path_tokens(text),
        }),
        ReferenceRelation::TypeUse => {
            if is_declaration_or_import_name(node) {
                return None;
            }
            Some(ReferenceDescriptor::Type {
                path: path_tokens(text),
            })
        }
        ReferenceRelation::Inherits => Some(ReferenceDescriptor::Type {
            path: path_tokens(text),
        }),
        ReferenceRelation::Implements if language == SourceLanguage::Rust => {
            let trait_node = node.child_by_field_name("trait")?;
            let type_node = node.child_by_field_name("type")?;
            Some(ReferenceDescriptor::Implementation {
                trait_path: path_tokens(content.get(trait_node.byte_range()).unwrap_or_default()),
                for_type_path: path_tokens(content.get(type_node.byte_range()).unwrap_or_default()),
            })
        }
        ReferenceRelation::Implements => Some(ReferenceDescriptor::Type {
            path: path_tokens(text),
        }),
        ReferenceRelation::Lexical => None,
    }
    .filter(descriptor_has_target)
}

fn descriptor_has_target(descriptor: &ReferenceDescriptor) -> bool {
    match descriptor {
        ReferenceDescriptor::Module { specifier, .. } => !specifier.is_empty(),
        ReferenceDescriptor::Symbol { path } | ReferenceDescriptor::Type { path } => {
            !path.is_empty()
        }
        ReferenceDescriptor::Implementation { trait_path, .. } => !trait_path.is_empty(),
    }
}

fn is_declaration_or_import_name(node: tree_sitter::Node<'_>) -> bool {
    let mut current = Some(node);
    while let Some(candidate) = current {
        if matches!(
            candidate.kind(),
            "use_declaration"
                | "import_statement"
                | "export_statement"
                | "impl_item"
                | "extends_clause"
                | "extends_type_clause"
                | "implements_clause"
        ) {
            return true;
        }
        if let Some(parent) = candidate.parent()
            && parent
                .child_by_field_name("name")
                .is_some_and(|name| name.id() == candidate.id())
        {
            return true;
        }
        current = candidate.parent();
    }
    false
}

fn path_tokens(text: &str) -> Vec<String> {
    text.split(|character: char| {
        !character.is_ascii_alphanumeric() && character != '_' && character != '$'
    })
    .filter(|token| {
        !token.is_empty()
            && !matches!(
                *token,
                "extends" | "implements" | "type" | "dyn" | "mut" | "ref"
            )
    })
    .map(str::to_string)
    .collect()
}

fn import_bindings(language: SourceLanguage, text: &str) -> Vec<ReferenceBinding> {
    match language {
        SourceLanguage::Rust => rust_import_bindings(text),
        SourceLanguage::TypeScript | SourceLanguage::Tsx => typescript_import_bindings(text),
        SourceLanguage::Unsupported => Vec::new(),
    }
}

fn rust_import_bindings(text: &str) -> Vec<ReferenceBinding> {
    let body = text
        .trim_start_matches("pub ")
        .trim_start_matches("use ")
        .trim_end_matches(';')
        .trim();
    let mut bindings = Vec::new();
    let terminal = body
        .rsplit("::")
        .next()
        .unwrap_or(body)
        .trim_matches(['{', '}']);
    for item in terminal
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let (imported, local) = item
            .split_once(" as ")
            .map_or((item, item), |(imported, local)| {
                (imported.trim(), local.trim())
            });
        bindings.push(ReferenceBinding {
            imported: imported.to_string(),
            local: local.to_string(),
            kind: if imported == "*" {
                ReferenceBindingKind::Glob
            } else {
                ReferenceBindingKind::Named
            },
        });
    }
    bindings
}

fn typescript_import_bindings(text: &str) -> Vec<ReferenceBinding> {
    let head = text
        .split_once(" from ")
        .map_or(text, |(head, _)| head)
        .trim_start_matches("import ")
        .trim_start_matches("export ")
        .trim_start_matches("type ")
        .trim();
    let mut bindings = Vec::new();
    if let Some(namespace) = head.strip_prefix("* as ") {
        bindings.push(ReferenceBinding {
            imported: "*".into(),
            local: namespace.trim().to_string(),
            kind: ReferenceBindingKind::Namespace,
        });
        return bindings;
    }
    let named = head
        .split_once('{')
        .and_then(|(_, tail)| tail.split_once('}').map(|(items, _)| items));
    if let Some(named) = named {
        for item in named
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            let item = item.trim_start_matches("type ");
            let (imported, local) = item
                .split_once(" as ")
                .map_or((item, item), |(imported, local)| {
                    (imported.trim(), local.trim())
                });
            bindings.push(ReferenceBinding {
                imported: imported.to_string(),
                local: local.to_string(),
                kind: if head.starts_with("type ") {
                    ReferenceBindingKind::Type
                } else {
                    ReferenceBindingKind::Named
                },
            });
        }
    }
    let default = head.split(',').next().unwrap_or_default().trim();
    if !default.is_empty() && !default.starts_with('{') && !default.starts_with('*') {
        bindings.push(ReferenceBinding {
            imported: "default".into(),
            local: default.to_string(),
            kind: ReferenceBindingKind::Default,
        });
    }
    bindings
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
                reference_key: None,
                descriptor: None,
                source_symbol_key: None,
                target_symbol_key: None,
                target_revision: None,
                resolution_evidence: None,
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
    fn v2_emits_normalized_reexports_and_type_relations() {
        let extractor = TreeSitterExtractor;
        let rust = source(
            "src/session.rs",
            "pub use crate::auth::Token as SessionToken;\npub struct Session { token: SessionToken }\nimpl crate::auth::Authenticatable for Session {}\n",
        );
        let typescript = source(
            "web/session.ts",
            "import type { Token as AuthToken } from './auth';\nexport { api } from './api';\ninterface SessionContract extends BaseContract {}\nexport class Session extends Parent implements SessionContract { token!: AuthToken; }\n",
        );
        let facts = extractor
            .extract(
                &[rust.clone(), typescript.clone()],
                CollectionBudget::default(),
                &CancellationToken::new(),
            )
            .unwrap();

        assert!(facts.symbols.iter().all(|symbol| {
            symbol
                .symbol_key
                .as_deref()
                .is_some_and(|key| key.len() == 64)
                && symbol
                    .qualified_name
                    .as_deref()
                    .is_some_and(|name| name.ends_with(&symbol.name))
        }));
        for relation in [
            ReferenceRelation::Import,
            ReferenceRelation::Reexport,
            ReferenceRelation::TypeUse,
            ReferenceRelation::Inherits,
            ReferenceRelation::Implements,
        ] {
            assert!(
                facts
                    .references
                    .iter()
                    .any(|reference| reference.relation == relation),
                "missing {relation:?}"
            );
        }
        assert!(facts.references.iter().all(|reference| {
            reference
                .reference_key
                .as_deref()
                .is_some_and(|key| key.len() == 64)
                && reference.descriptor.is_some()
        }));
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
