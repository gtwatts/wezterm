//! Semantic bridge: wires elwood-core's tree-sitter intelligence into the bridge.
//!
//! Provides code-aware completions, `@symbol:name` references, and
//! project-aware agent context enrichment via TF-IDF similarity search.
//!
//! ## Features
//!
//! - **Symbol completions**: Fuzzy-match function/class/variable names from
//!   the project's `SymbolIndex` for ghost-text suggestions.
//! - **`@symbol:name` resolution**: Resolve a symbol name to its definition
//!   source code for context attachment.
//! - **Relevant context retrieval**: Before each agent turn, find code snippets
//!   related to the user's message via TF-IDF cosine similarity.

use elwood_core::treesitter::semantic::{SemanticIndex, TfIdfEmbedder};
use elwood_core::treesitter::{LanguageRegistry, Symbol, SymbolIndex, SymbolKind};
use std::path::{Path, PathBuf};

/// Default TF-IDF embedding dimension.
const TFIDF_DIMENSIONS: usize = 128;

/// Maximum number of files to index (prevents runaway on huge repos).
const MAX_INDEX_FILES: usize = 5_000;

/// Maximum file size to index (512 KB).
const MAX_FILE_SIZE: u64 = 512 * 1024;

/// Maximum depth when walking the project directory.
const MAX_WALK_DEPTH: usize = 10;

/// Minimum TF-IDF similarity score to include in context results.
const MIN_RELEVANCE_SCORE: f32 = 0.1;

/// A symbol completion candidate.
#[derive(Debug, Clone)]
pub struct SymbolCompletion {
    /// The symbol name.
    pub name: String,
    /// Kind of symbol (function, struct, etc.).
    pub kind: SymbolKind,
    /// File where the symbol is defined (relative to project root).
    pub file: String,
    /// Line number of the definition.
    pub line: usize,
    /// Match score (higher = better).
    pub score: f32,
}

/// A resolved symbol definition with source code.
#[derive(Debug, Clone)]
pub struct SymbolDefinition {
    /// The symbol name.
    pub name: String,
    /// Kind of symbol.
    pub kind: SymbolKind,
    /// Absolute file path.
    pub file: PathBuf,
    /// Start line (1-based).
    pub start_line: usize,
    /// End line (1-based).
    pub end_line: usize,
    /// The symbol's signature line.
    pub signature: String,
    /// The full source code of the symbol definition.
    pub source: String,
}

/// A code snippet found by semantic similarity search.
#[derive(Debug, Clone)]
pub struct ContextSnippet {
    /// Identifier (file:line).
    pub id: String,
    /// The source text of the snippet.
    pub text: String,
    /// Cosine similarity score.
    pub score: f32,
}

/// Bridge between elwood-core's tree-sitter capabilities and the terminal pane.
///
/// Lazily initializes by scanning the project directory and building a
/// `SymbolIndex` + `SemanticIndex` on a background thread.
pub struct SemanticBridge {
    symbol_index: Option<SymbolIndex>,
    semantic_index: SemanticIndex,
    embedder: TfIdfEmbedder,
    project_root: PathBuf,
    initialized: bool,
}

impl SemanticBridge {
    /// Create a new semantic bridge for the given project root.
    ///
    /// The bridge is not initialized until [`initialize()`](Self::initialize) is called.
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            symbol_index: None,
            semantic_index: SemanticIndex::new(),
            embedder: TfIdfEmbedder::new(TFIDF_DIMENSIONS),
            project_root,
            initialized: false,
        }
    }

    /// Initialize the bridge by scanning the project and building indices.
    ///
    /// This is a blocking operation that walks the project directory,
    /// parses source files with tree-sitter, and indexes symbols for
    /// semantic search. Should be called from a background thread.
    pub fn initialize(&mut self) {
        if self.initialized {
            return;
        }

        let registry = LanguageRegistry::new();
        let mut index = SymbolIndex::new(registry);

        let paths = collect_source_files(&self.project_root);
        if !paths.is_empty() {
            let parsed = index.parse_files_parallel(&paths);
            tracing::info!(
                "SemanticBridge: indexed {} files ({} symbols) from {}",
                parsed,
                index.symbol_count(),
                self.project_root.display()
            );
        }

        // Build semantic index from all symbols
        for symbol in index.all_symbols() {
            let id = format!("{}:{}", symbol.file.display(), symbol.start_line);
            let text = format!(
                "{} {} {} {}",
                symbol.kind,
                symbol.name,
                symbol.signature,
                symbol.parent.as_deref().unwrap_or("")
            );
            self.semantic_index.add(&id, &text, &self.embedder);
        }

        self.symbol_index = Some(index);
        self.initialized = true;
    }

    /// Whether the bridge has been initialized.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Complete a symbol name prefix.
    ///
    /// Returns up to `limit` symbols whose names match the prefix
    /// (case-insensitive), ranked by relevance.
    pub fn complete_symbol(&self, prefix: &str, limit: usize) -> Vec<SymbolCompletion> {
        let index = match &self.symbol_index {
            Some(idx) => idx,
            None => return Vec::new(),
        };

        if prefix.is_empty() {
            return Vec::new();
        }

        let prefix_lower = prefix.to_lowercase();
        let mut results: Vec<SymbolCompletion> = index
            .all_symbols()
            .into_iter()
            .filter_map(|sym| {
                let name_lower = sym.name.to_lowercase();
                let score = if name_lower == prefix_lower {
                    // Exact match
                    100.0
                } else if name_lower.starts_with(&prefix_lower) {
                    // Prefix match — shorter names score higher
                    50.0 + (1.0 / sym.name.len() as f32) * 10.0
                } else if name_lower.contains(&prefix_lower) {
                    // Substring match
                    20.0
                } else {
                    return None;
                };

                let relative_file = sym
                    .file
                    .strip_prefix(&self.project_root)
                    .unwrap_or(&sym.file)
                    .display()
                    .to_string();

                Some(SymbolCompletion {
                    name: sym.name.clone(),
                    kind: sym.kind,
                    file: relative_file,
                    line: sym.start_line,
                    score,
                })
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        results
    }

    /// Resolve a symbol name to its definition and source code.
    ///
    /// Finds the best-matching symbol by name and reads the source lines
    /// from disk. Returns `None` if no match is found.
    pub fn resolve_symbol(&self, name: &str) -> Option<SymbolDefinition> {
        let index = self.symbol_index.as_ref()?;

        let matches = index.search_by_name(name);
        // Prefer exact match, then take first result
        let symbol = matches
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .or_else(|| matches.first())?;

        let source = read_symbol_source(symbol)?;

        Some(SymbolDefinition {
            name: symbol.name.clone(),
            kind: symbol.kind,
            file: symbol.file.clone(),
            start_line: symbol.start_line,
            end_line: symbol.end_line,
            signature: symbol.signature.clone(),
            source,
        })
    }

    /// Find code context relevant to a query string.
    ///
    /// Uses TF-IDF similarity to find the most related symbols, then reads
    /// their source code. Stops when `max_tokens` (approximate) is reached.
    pub fn find_relevant_context(
        &self,
        query: &str,
        max_tokens: usize,
    ) -> Vec<ContextSnippet> {
        if !self.initialized || query.is_empty() {
            return Vec::new();
        }

        let results = self.semantic_index.search(query, 20, &self.embedder);
        let mut snippets = Vec::new();
        let mut token_budget = max_tokens;

        for result in results {
            if result.score < MIN_RELEVANCE_SCORE {
                break;
            }

            // Parse the id to get file:line
            let parts: Vec<&str> = result.id.rsplitn(2, ':').collect();
            if parts.len() != 2 {
                continue;
            }
            let line_str = parts[0];
            let file_str = parts[1];

            let line: usize = match line_str.parse() {
                Ok(l) => l,
                Err(_) => continue,
            };

            // Find the symbol to get end_line
            let symbol = self
                .symbol_index
                .as_ref()
                .and_then(|idx| {
                    idx.all_symbols().into_iter().find(|s| {
                        s.file.display().to_string() == file_str && s.start_line == line
                    })
                });

            let source = match symbol {
                Some(sym) => match read_symbol_source(sym) {
                    Some(s) => s,
                    None => continue,
                },
                None => continue,
            };

            // Approximate token count (4 chars per token)
            let approx_tokens = source.len() / 4;
            if approx_tokens > token_budget {
                break;
            }
            token_budget = token_budget.saturating_sub(approx_tokens);

            snippets.push(ContextSnippet {
                id: result.id,
                text: source,
                score: result.score,
            });
        }

        snippets
    }

    /// Refresh the index for changed files.
    ///
    /// Re-scans the project directory and reparses any files that have
    /// changed. This is more efficient than a full rebuild.
    pub fn refresh(&mut self) {
        let index = match &mut self.symbol_index {
            Some(idx) => idx,
            None => return,
        };

        let paths = collect_source_files(&self.project_root);
        for path in &paths {
            if index.is_indexed(path) {
                index.reparse_file(path);
            } else {
                index.parse_file(path);
            }
        }

        // Rebuild semantic index
        self.semantic_index.clear();
        for symbol in index.all_symbols() {
            let id = format!("{}:{}", symbol.file.display(), symbol.start_line);
            let text = format!(
                "{} {} {} {}",
                symbol.kind,
                symbol.name,
                symbol.signature,
                symbol.parent.as_deref().unwrap_or("")
            );
            self.semantic_index.add(&id, &text, &self.embedder);
        }

        tracing::debug!(
            "SemanticBridge: refreshed — {} files, {} symbols",
            index.file_count(),
            index.symbol_count()
        );
    }

    /// Get the number of indexed symbols.
    pub fn symbol_count(&self) -> usize {
        self.symbol_index
            .as_ref()
            .map(|idx| idx.symbol_count())
            .unwrap_or(0)
    }

    /// Get the number of indexed files.
    pub fn file_count(&self) -> usize {
        self.symbol_index
            .as_ref()
            .map(|idx| idx.file_count())
            .unwrap_or(0)
    }
}

/// Read the source code lines for a symbol from disk.
fn read_symbol_source(symbol: &Symbol) -> Option<String> {
    let content = std::fs::read_to_string(&symbol.file).ok()?;
    let lines: Vec<&str> = content.lines().collect();

    let start = symbol.start_line.saturating_sub(1); // 1-based to 0-based
    let end = symbol.end_line.min(lines.len());

    if start >= lines.len() || start >= end {
        return None;
    }

    Some(lines[start..end].join("\n"))
}

/// Collect source files from a project directory, respecting .gitignore.
fn collect_source_files(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .max_depth(Some(MAX_WALK_DEPTH))
        .build();

    // Pre-build the extension set for fast lookups
    let supported_exts: std::collections::HashSet<&str> = [
        "rs", "py", "pyi", "js", "jsx", "mjs", "cjs", "ts", "tsx", "go", "c", "h", "cpp", "cc",
        "cxx", "hpp", "hh", "hxx", "java", "rb", "sh", "bash", "zsh",
    ]
    .into_iter()
    .collect();

    for entry in walker.flatten() {
        if paths.len() >= MAX_INDEX_FILES {
            break;
        }

        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }

        // Check file extension
        let ext = entry
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !supported_exts.contains(ext) {
            continue;
        }

        // Skip oversized files
        if let Ok(meta) = entry.metadata() {
            if meta.len() > MAX_FILE_SIZE {
                continue;
            }
        }

        paths.push(entry.into_path());
    }

    paths
}

/// Check if a string looks like a `@symbol:name` reference.
pub fn is_symbol_reference(token: &str) -> bool {
    token.starts_with("symbol:") && token.len() > 7
}

/// Extract the symbol name from a `@symbol:name` token.
pub fn extract_symbol_name(token: &str) -> Option<&str> {
    token.strip_prefix("symbol:")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_test_project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("create temp dir");
        let base = dir.path();

        fs::create_dir_all(base.join("src")).unwrap();

        fs::write(
            base.join("src/main.rs"),
            r#"
fn main() {
    println!("Hello!");
}

pub fn calculate_total(items: &[i32]) -> i32 {
    items.iter().sum()
}

pub struct Config {
    pub name: String,
    pub value: i32,
}

impl Config {
    pub fn new(name: &str, value: i32) -> Self {
        Config {
            name: name.to_string(),
            value,
        }
    }
}
"#,
        )
        .unwrap();

        fs::write(
            base.join("src/lib.rs"),
            r#"
pub fn process_data(data: &str) -> String {
    data.to_uppercase()
}

pub fn validate_input(input: &str) -> bool {
    !input.is_empty()
}
"#,
        )
        .unwrap();

        dir
    }

    #[test]
    fn test_new_bridge_not_initialized() {
        let bridge = SemanticBridge::new(PathBuf::from("/tmp/test"));
        assert!(!bridge.is_initialized());
        assert_eq!(bridge.symbol_count(), 0);
        assert_eq!(bridge.file_count(), 0);
    }

    #[test]
    fn test_initialize_indexes_files() {
        let dir = setup_test_project();
        let mut bridge = SemanticBridge::new(dir.path().to_path_buf());
        bridge.initialize();

        assert!(bridge.is_initialized());
        assert!(bridge.file_count() >= 2);
        assert!(bridge.symbol_count() >= 4); // main, calculate_total, Config, new, process_data, validate_input
    }

    #[test]
    fn test_complete_symbol_prefix() {
        let dir = setup_test_project();
        let mut bridge = SemanticBridge::new(dir.path().to_path_buf());
        bridge.initialize();

        let results = bridge.complete_symbol("calc", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "calculate_total");
    }

    #[test]
    fn test_complete_symbol_case_insensitive() {
        let dir = setup_test_project();
        let mut bridge = SemanticBridge::new(dir.path().to_path_buf());
        bridge.initialize();

        let results = bridge.complete_symbol("CONFIG", 5);
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.name == "Config"));
    }

    #[test]
    fn test_complete_symbol_empty_prefix() {
        let dir = setup_test_project();
        let mut bridge = SemanticBridge::new(dir.path().to_path_buf());
        bridge.initialize();

        let results = bridge.complete_symbol("", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_complete_symbol_no_match() {
        let dir = setup_test_project();
        let mut bridge = SemanticBridge::new(dir.path().to_path_buf());
        bridge.initialize();

        let results = bridge.complete_symbol("zzzznonexistent", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_complete_symbol_not_initialized() {
        let bridge = SemanticBridge::new(PathBuf::from("/tmp/test"));
        let results = bridge.complete_symbol("calc", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_resolve_symbol() {
        let dir = setup_test_project();
        let mut bridge = SemanticBridge::new(dir.path().to_path_buf());
        bridge.initialize();

        let def = bridge.resolve_symbol("calculate_total");
        assert!(def.is_some());
        let def = def.unwrap();
        assert_eq!(def.name, "calculate_total");
        assert_eq!(def.kind, SymbolKind::Function);
        assert!(def.source.contains("items.iter().sum()"));
    }

    #[test]
    fn test_resolve_symbol_not_found() {
        let dir = setup_test_project();
        let mut bridge = SemanticBridge::new(dir.path().to_path_buf());
        bridge.initialize();

        let def = bridge.resolve_symbol("nonexistent_function_xyz");
        assert!(def.is_none());
    }

    #[test]
    fn test_find_relevant_context() {
        let dir = setup_test_project();
        let mut bridge = SemanticBridge::new(dir.path().to_path_buf());
        bridge.initialize();

        let snippets = bridge.find_relevant_context("calculate total items", 4096);
        // Should find something related to calculate_total
        assert!(!snippets.is_empty());
        assert!(snippets[0].score >= MIN_RELEVANCE_SCORE);
    }

    #[test]
    fn test_find_relevant_context_empty_query() {
        let dir = setup_test_project();
        let mut bridge = SemanticBridge::new(dir.path().to_path_buf());
        bridge.initialize();

        let snippets = bridge.find_relevant_context("", 4096);
        assert!(snippets.is_empty());
    }

    #[test]
    fn test_find_relevant_context_not_initialized() {
        let bridge = SemanticBridge::new(PathBuf::from("/tmp/test"));
        let snippets = bridge.find_relevant_context("test", 4096);
        assert!(snippets.is_empty());
    }

    #[test]
    fn test_refresh() {
        let dir = setup_test_project();
        let mut bridge = SemanticBridge::new(dir.path().to_path_buf());
        bridge.initialize();

        let initial_count = bridge.symbol_count();

        // Add a new file
        fs::write(
            dir.path().join("src/extra.rs"),
            "pub fn extra_function() {}\n",
        )
        .unwrap();

        bridge.refresh();
        assert!(bridge.symbol_count() > initial_count);
    }

    #[test]
    fn test_is_symbol_reference() {
        assert!(is_symbol_reference("symbol:calculate_total"));
        assert!(is_symbol_reference("symbol:Config"));
        assert!(!is_symbol_reference("symbol:"));
        assert!(!is_symbol_reference("file:main.rs"));
        assert!(!is_symbol_reference("calculate_total"));
    }

    #[test]
    fn test_extract_symbol_name() {
        assert_eq!(extract_symbol_name("symbol:calculate_total"), Some("calculate_total"));
        assert_eq!(extract_symbol_name("symbol:Config"), Some("Config"));
        assert_eq!(extract_symbol_name("not_a_symbol"), None);
    }

    #[test]
    fn test_collect_source_files() {
        let dir = setup_test_project();
        let files = collect_source_files(dir.path());
        assert!(files.len() >= 2);
        assert!(files.iter().all(|p| p.extension().is_some()));
    }

    #[test]
    fn test_collect_source_files_skips_unsupported() {
        let dir = setup_test_project();
        fs::write(dir.path().join("readme.txt"), "not code").unwrap();
        fs::write(dir.path().join("data.json"), "{}").unwrap();

        let files = collect_source_files(dir.path());
        assert!(files.iter().all(|p| {
            let ext = p.extension().unwrap().to_str().unwrap();
            ext == "rs" // Only .rs files in our test project
        }));
    }

    #[test]
    fn test_double_initialize_idempotent() {
        let dir = setup_test_project();
        let mut bridge = SemanticBridge::new(dir.path().to_path_buf());
        bridge.initialize();
        let count = bridge.symbol_count();
        bridge.initialize(); // second call should be no-op
        assert_eq!(bridge.symbol_count(), count);
    }
}
