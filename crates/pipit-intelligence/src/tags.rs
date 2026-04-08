use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCapture, QueryCursor};

/// A tag is a named symbol found in a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTag {
    pub rel_path: PathBuf,
    pub line: u32,
    pub name: String,
    pub kind: TagKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TagKind {
    Definition,
    Reference,
}

/// Extract tags from all files in parallel using rayon.
pub fn extract_all_tags(root: &Path, files: &[PathBuf]) -> Vec<FileTag> {
    files
        .par_iter()
        .filter_map(|rel_path| {
            let abs_path = root.join(rel_path);
            let source = std::fs::read_to_string(&abs_path).ok()?;
            let lang = crate::discovery::detect_language(rel_path)?;
            Some(extract_tags_from_source(rel_path, &source, lang))
        })
        .flatten()
        .collect()
}

fn extract_tags_from_source(rel_path: &Path, source: &str, lang: &str) -> Vec<FileTag> {
    let Some(language_support) = language_support(lang) else {
        return Vec::new();
    };

    let mut parser = Parser::new();
    let language = language_support.language();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };

    let mut tags = Vec::new();
    let mut seen_definitions = HashSet::new();
    let mut definition_names = HashSet::new();

    for query_source in language_support.definition_queries {
        if let Ok(query) = Query::new(&language, query_source) {
            let mut cursor = QueryCursor::new();
            let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
            while let Some(query_match) = matches.next() {
                for capture in query_match.captures {
                    if let Some(tag) =
                        capture_to_tag(rel_path, source, *capture, TagKind::Definition)
                    {
                        let key = (tag.line, tag.name.clone());
                        if seen_definitions.insert(key) {
                            definition_names.insert(tag.name.clone());
                            tags.push(tag);
                        }
                    }
                }
            }
        }
    }

    let mut seen_references = HashSet::new();
    for query_source in language_support.reference_queries {
        if let Ok(query) = Query::new(&language, query_source) {
            let mut cursor = QueryCursor::new();
            let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
            while let Some(query_match) = matches.next() {
                for capture in query_match.captures {
                    if let Some(tag) =
                        capture_to_tag(rel_path, source, *capture, TagKind::Reference)
                    {
                        if definition_names.contains(&tag.name) || is_noise_identifier(&tag.name) {
                            continue;
                        }

                        let key = (tag.line, tag.name.clone());
                        if seen_references.insert(key) {
                            tags.push(tag);
                        }
                    }
                }
            }
        }
    }

    tags
}

struct LanguageSupport {
    language: fn() -> Language,
    definition_queries: &'static [&'static str],
    reference_queries: &'static [&'static str],
}

impl LanguageSupport {
    fn language(&self) -> Language {
        (self.language)()
    }
}

fn language_support(lang: &str) -> Option<LanguageSupport> {
    match lang {
        "rust" => Some(LanguageSupport {
            language: rust_language,
            definition_queries: &[
                "(function_item name: (identifier) @name)",
                "(struct_item name: (type_identifier) @name)",
                "(enum_item name: (type_identifier) @name)",
                "(trait_item name: (type_identifier) @name)",
                "(type_item name: (type_identifier) @name)",
                "(const_item name: (identifier) @name)",
                "(static_item name: (identifier) @name)",
                "(mod_item name: (identifier) @name)",
            ],
            reference_queries: &["(identifier) @name", "(type_identifier) @name"],
        }),
        "python" => Some(LanguageSupport {
            language: python_language,
            definition_queries: &[
                "(function_definition name: (identifier) @name)",
                "(class_definition name: (identifier) @name)",
            ],
            reference_queries: &["(identifier) @name"],
        }),
        "javascript" => Some(LanguageSupport {
            language: javascript_language,
            definition_queries: &[
                "(function_declaration name: (identifier) @name)",
                "(class_declaration name: (identifier) @name)",
                "(variable_declarator name: (identifier) @name)",
            ],
            reference_queries: &["(identifier) @name"],
        }),
        "typescript" => Some(LanguageSupport {
            language: typescript_language,
            definition_queries: &[
                "(function_declaration name: (identifier) @name)",
                "(class_declaration name: (type_identifier) @name)",
                "(interface_declaration name: (type_identifier) @name)",
                "(type_alias_declaration name: (type_identifier) @name)",
                "(enum_declaration name: (identifier) @name)",
                "(variable_declarator name: (identifier) @name)",
            ],
            reference_queries: &["(identifier) @name", "(type_identifier) @name"],
        }),
        "go" => Some(LanguageSupport {
            language: go_language,
            definition_queries: &[
                "(function_declaration name: (identifier) @name)",
                "(method_declaration name: (field_identifier) @name)",
                "(type_spec name: (type_identifier) @name)",
            ],
            reference_queries: &[
                "(identifier) @name",
                "(type_identifier) @name",
                "(field_identifier) @name",
            ],
        }),
        _ => None,
    }
}

fn capture_to_tag(
    rel_path: &Path,
    source: &str,
    capture: QueryCapture<'_>,
    kind: TagKind,
) -> Option<FileTag> {
    let text = capture.node.utf8_text(source.as_bytes()).ok()?.trim();
    if !looks_like_identifier(text) || is_noise_identifier(text) {
        return None;
    }

    Some(FileTag {
        rel_path: rel_path.to_path_buf(),
        line: capture.node.start_position().row as u32 + 1,
        name: text.to_string(),
        kind,
    })
}

fn rust_language() -> Language {
    tree_sitter_rust::LANGUAGE.into()
}

fn python_language() -> Language {
    tree_sitter_python::LANGUAGE.into()
}

fn javascript_language() -> Language {
    tree_sitter_javascript::LANGUAGE.into()
}

fn typescript_language() -> Language {
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}

fn go_language() -> Language {
    tree_sitter_go::LANGUAGE.into()
}

fn is_noise_identifier(name: &str) -> bool {
    matches!(
        name,
        "new"
            | "self"
            | "Self"
            | "main"
            | "test"
            | "it"
            | "describe"
            | "if"
            | "else"
            | "for"
            | "while"
            | "return"
            | "true"
            | "false"
            | "None"
            | "Some"
            | "Ok"
            | "Err"
            | "default"
            | "impl"
            | "pub"
    )
}

fn looks_like_identifier(word: &str) -> bool {
    let first = word.chars().next().unwrap_or('0');
    (first.is_alphabetic() || first == '_')
        && word.chars().all(|c| c.is_alphanumeric() || c == '_')
        && word.len() >= 2
        && word.len() <= 64
}
