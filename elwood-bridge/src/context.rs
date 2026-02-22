//! @ context attachment system for the Elwood pane.
//!
//! When the user types `@filename` in their prompt, the system searches for
//! matching files in the working directory (respecting `.gitignore`) and
//! attaches file contents to the prompt sent to the agent.
//!
//! ## File Search
//!
//! Uses the `ignore` crate (same as ripgrep) for fast, `.gitignore`-aware
//! file walking. Files larger than [`MAX_FILE_SIZE`] are skipped.
//!
//! ## Usage
//!
//! ```text
//! @src/main.rs explain this file        -> attaches src/main.rs
//! @Cargo.toml @README.md compare these  -> attaches both files
//! ```

use std::path::{Path, PathBuf};

/// Maximum file size to attach (100 KB).
const MAX_FILE_SIZE: u64 = 100 * 1024;

/// Maximum number of search results to return.
const MAX_SEARCH_RESULTS: usize = 20;

/// A file attachment parsed from an `@` reference.
#[derive(Debug, Clone)]
pub struct ContextAttachment {
    /// Display label (e.g. `@src/main.rs`).
    pub label: String,
    /// The resolved absolute path.
    pub path: PathBuf,
    /// File contents (may be truncated for very large files).
    pub content: String,
}

/// Search for files matching `query` under `cwd`, respecting `.gitignore`.
///
/// Returns up to [`MAX_SEARCH_RESULTS`] matching paths, sorted by relevance
/// (exact basename match first, then prefix match, then substring).
pub fn search_files(query: &str, cwd: &Path) -> Vec<PathBuf> {
    if query.is_empty() {
        return Vec::new();
    }

    let query_lower = query.to_lowercase();
    let mut results = Vec::new();

    // Use ignore crate's WalkBuilder for .gitignore-aware traversal
    let walker = ignore::WalkBuilder::new(cwd)
        .hidden(true)         // skip hidden files by default
        .git_ignore(true)     // respect .gitignore
        .git_global(true)     // respect global gitignore
        .git_exclude(true)    // respect .git/info/exclude
        .max_depth(Some(8))   // don't recurse too deep
        .build();

    for entry in walker.flatten() {
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }

        // Check file size
        if let Ok(meta) = entry.metadata() {
            if meta.len() > MAX_FILE_SIZE {
                continue;
            }
        }

        let path = entry.path();
        let relative = path.strip_prefix(cwd).unwrap_or(path);
        let rel_str = relative.to_string_lossy().to_lowercase();
        let basename = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        // Match: exact path, basename match, or substring
        if rel_str == query_lower
            || basename == query_lower
            || rel_str.contains(&query_lower)
            || basename.contains(&query_lower)
        {
            results.push((path.to_path_buf(), relative.to_path_buf()));
        }

        if results.len() >= MAX_SEARCH_RESULTS * 2 {
            break; // collect enough candidates
        }
    }

    // Sort by relevance: exact match > basename match > path substring
    results.sort_by(|(_, rel_a), (_, rel_b)| {
        let a_str = rel_a.to_string_lossy().to_lowercase();
        let b_str = rel_b.to_string_lossy().to_lowercase();
        let a_base = rel_a
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let b_base = rel_b
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        let a_exact = a_str == query_lower;
        let b_exact = b_str == query_lower;
        let a_base_match = a_base == query_lower;
        let b_base_match = b_base == query_lower;

        // Exact path match first
        if a_exact != b_exact {
            return b_exact.cmp(&a_exact);
        }
        // Basename match next
        if a_base_match != b_base_match {
            return b_base_match.cmp(&a_base_match);
        }
        // Shorter paths preferred (closer to root)
        a_str.len().cmp(&b_str.len())
    });

    results
        .into_iter()
        .take(MAX_SEARCH_RESULTS)
        .map(|(abs, _)| abs)
        .collect()
}

/// Read a file and create a [`ContextAttachment`].
///
/// Returns `None` if the file doesn't exist, is too large, or can't be read as UTF-8.
pub fn read_attachment(path: &Path, cwd: &Path) -> Option<ContextAttachment> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > MAX_FILE_SIZE {
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    let relative = path.strip_prefix(cwd).unwrap_or(path);
    Some(ContextAttachment {
        label: format!("@{}", relative.display()),
        path: path.to_path_buf(),
        content,
    })
}

/// Parse `@` references from a user prompt, returning (references, remaining_text).
///
/// Each `@` reference is a contiguous non-whitespace token starting with `@`.
/// The remaining text has the `@` tokens removed.
pub fn parse_at_references(input: &str) -> (Vec<String>, String) {
    let mut refs = Vec::new();
    let mut remaining = String::new();
    let mut first_word = true;

    for token in input.split_whitespace() {
        if token.starts_with('@') && token.len() > 1 {
            // Strip the leading '@'
            refs.push(token[1..].to_string());
        } else {
            if !first_word && !remaining.is_empty() {
                remaining.push(' ');
            }
            remaining.push_str(token);
            first_word = false;
        }
    }

    (refs, remaining)
}

/// Resolve `@` references to file attachments and build the augmented prompt.
///
/// Returns `(attachments, augmented_prompt)` where the augmented prompt has
/// file contents prepended as context blocks.
pub fn resolve_and_build_prompt(
    input: &str,
    cwd: &Path,
) -> (Vec<ContextAttachment>, String) {
    let (refs, user_text) = parse_at_references(input);
    if refs.is_empty() {
        return (Vec::new(), input.to_string());
    }

    let mut attachments = Vec::new();
    let mut context_block = String::new();

    for reference in &refs {
        // Try direct path resolution first
        let direct_path = cwd.join(reference);
        let attachment = if direct_path.is_file() {
            read_attachment(&direct_path, cwd)
        } else {
            // Fall back to search
            search_files(reference, cwd)
                .first()
                .and_then(|p| read_attachment(p, cwd))
        };

        if let Some(att) = attachment {
            context_block.push_str(&format!(
                "<file path=\"{}\">\n{}\n</file>\n\n",
                att.label.trim_start_matches('@'),
                att.content,
            ));
            attachments.push(att);
        }
    }

    let augmented = if context_block.is_empty() {
        user_text
    } else if user_text.is_empty() {
        context_block.trim_end().to_string()
    } else {
        format!("{context_block}{user_text}")
    };

    (attachments, augmented)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a temp directory with some test files.
    fn setup_test_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("create temp dir");
        let base = dir.path();

        // Create files
        fs::write(base.join("README.md"), "# Test Project\n").unwrap();
        fs::write(base.join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();

        // Create src directory with files
        fs::create_dir_all(base.join("src")).unwrap();
        fs::write(base.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(base.join("src/lib.rs"), "pub fn hello() {}\n").unwrap();

        // Create a large file (should be skipped)
        let large_content = "x".repeat(MAX_FILE_SIZE as usize + 1);
        fs::write(base.join("large_file.bin"), large_content).unwrap();

        dir
    }

    #[test]
    fn test_parse_at_references_basic() {
        let (refs, text) = parse_at_references("@src/main.rs explain this file");
        assert_eq!(refs, vec!["src/main.rs"]);
        assert_eq!(text, "explain this file");
    }

    #[test]
    fn test_parse_at_references_multiple() {
        let (refs, text) = parse_at_references("@Cargo.toml @README.md compare these");
        assert_eq!(refs, vec!["Cargo.toml", "README.md"]);
        assert_eq!(text, "compare these");
    }

    #[test]
    fn test_parse_at_references_none() {
        let (refs, text) = parse_at_references("no references here");
        assert!(refs.is_empty());
        assert_eq!(text, "no references here");
    }

    #[test]
    fn test_parse_at_references_only_refs() {
        let (refs, text) = parse_at_references("@file1.rs @file2.rs");
        assert_eq!(refs, vec!["file1.rs", "file2.rs"]);
        assert_eq!(text, "");
    }

    #[test]
    fn test_parse_at_references_bare_at() {
        // A bare `@` with nothing after it should not be treated as a reference
        let (refs, text) = parse_at_references("@ hello");
        assert!(refs.is_empty());
        assert_eq!(text, "@ hello");
    }

    #[test]
    fn test_search_files_by_name() {
        let dir = setup_test_dir();
        let results = search_files("main.rs", dir.path());
        assert!(!results.is_empty());
        assert!(results[0].to_string_lossy().contains("main.rs"));
    }

    #[test]
    fn test_search_files_by_path() {
        let dir = setup_test_dir();
        let results = search_files("src/main.rs", dir.path());
        assert!(!results.is_empty());
        assert!(results[0].to_string_lossy().contains("main.rs"));
    }

    #[test]
    fn test_search_files_empty_query() {
        let dir = setup_test_dir();
        let results = search_files("", dir.path());
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_files_no_match() {
        let dir = setup_test_dir();
        let results = search_files("nonexistent_file_xyz.txt", dir.path());
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_files_skips_large() {
        let dir = setup_test_dir();
        let results = search_files("large_file.bin", dir.path());
        assert!(results.is_empty());
    }

    #[test]
    fn test_read_attachment() {
        let dir = setup_test_dir();
        let path = dir.path().join("README.md");
        let att = read_attachment(&path, dir.path()).unwrap();
        assert_eq!(att.label, "@README.md");
        assert!(att.content.contains("Test Project"));
    }

    #[test]
    fn test_read_attachment_nonexistent() {
        let dir = setup_test_dir();
        let path = dir.path().join("nope.txt");
        assert!(read_attachment(&path, dir.path()).is_none());
    }

    #[test]
    fn test_resolve_and_build_prompt_with_files() {
        let dir = setup_test_dir();
        let input = "@README.md explain this";
        let (attachments, prompt) = resolve_and_build_prompt(input, dir.path());
        assert_eq!(attachments.len(), 1);
        assert!(prompt.contains("Test Project"));
        assert!(prompt.contains("explain this"));
    }

    #[test]
    fn test_resolve_and_build_prompt_no_refs() {
        let dir = setup_test_dir();
        let input = "just a message";
        let (attachments, prompt) = resolve_and_build_prompt(input, dir.path());
        assert!(attachments.is_empty());
        assert_eq!(prompt, "just a message");
    }

    #[test]
    fn test_resolve_and_build_prompt_missing_file() {
        let dir = setup_test_dir();
        let input = "@nonexistent.xyz explain";
        let (attachments, prompt) = resolve_and_build_prompt(input, dir.path());
        assert!(attachments.is_empty());
        assert_eq!(prompt, "explain");
    }
}
