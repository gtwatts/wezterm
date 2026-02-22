//! MCP resource providers — expose terminal state as MCP resources.
//!
//! Each resource provider handles a URI scheme and returns content for
//! `resources/list` and `resources/read` requests.

use super::protocol::{McpResourceDef, ResourceContent};

/// Query interface for accessing pane/session data from the MCP server.
///
/// The MCP server holds a sender side; the domain/pane side holds the receiver
/// and fulfills queries by reading terminal state.
#[derive(Debug, Clone)]
pub enum PaneQuery {
    /// Get visible content from a pane (last N lines).
    GetPaneContent {
        pane_id: Option<u64>,
        lines: usize,
    },
    /// Get the session log as markdown.
    GetSessionLog,
    /// Get git status as JSON.
    GetGitStatus,
    /// Get recent command history.
    GetCommandHistory { limit: usize },
    /// List all panes with IDs and titles.
    ListPanes,
}

/// Result of a pane query.
#[derive(Debug, Clone)]
pub struct PaneQueryResult {
    /// The content text.
    pub content: String,
    /// MIME type of the content.
    pub mime_type: String,
}

/// Static resource definitions exposed by the MCP server.
///
/// Returns the list of all resources the server advertises.
pub fn list_resources() -> Vec<McpResourceDef> {
    vec![
        McpResourceDef {
            uri: "terminal://pane/content".to_string(),
            name: "Terminal Pane Content".to_string(),
            description: Some(
                "Current visible text content of the active terminal pane (last 100 lines)"
                    .to_string(),
            ),
            mime_type: Some("text/plain".to_string()),
        },
        McpResourceDef {
            uri: "terminal://pane/list".to_string(),
            name: "Terminal Pane List".to_string(),
            description: Some("List all terminal panes with IDs and titles".to_string()),
            mime_type: Some("application/json".to_string()),
        },
        McpResourceDef {
            uri: "elwood://session/log".to_string(),
            name: "Session Log".to_string(),
            description: Some(
                "Full session log of agent messages, tool calls, and commands as markdown"
                    .to_string(),
            ),
            mime_type: Some("text/markdown".to_string()),
        },
        McpResourceDef {
            uri: "elwood://git/status".to_string(),
            name: "Git Status".to_string(),
            description: Some(
                "Current git branch, modified files, staged changes as JSON".to_string(),
            ),
            mime_type: Some("application/json".to_string()),
        },
        McpResourceDef {
            uri: "elwood://commands/history".to_string(),
            name: "Command History".to_string(),
            description: Some("Last 50 shell commands executed in the terminal".to_string()),
            mime_type: Some("application/json".to_string()),
        },
    ]
}

/// Resolve a resource URI to a `PaneQuery`.
///
/// Returns `None` if the URI doesn't match any known resource.
pub fn resolve_uri(uri: &str) -> Option<PaneQuery> {
    match uri {
        "terminal://pane/content" => Some(PaneQuery::GetPaneContent {
            pane_id: None,
            lines: 100,
        }),
        "terminal://pane/list" => Some(PaneQuery::ListPanes),
        "elwood://session/log" => Some(PaneQuery::GetSessionLog),
        "elwood://git/status" => Some(PaneQuery::GetGitStatus),
        "elwood://commands/history" => Some(PaneQuery::GetCommandHistory { limit: 50 }),
        _ => {
            // Handle parameterized URIs like terminal://pane/{id}/content
            if let Some(rest) = uri.strip_prefix("terminal://pane/") {
                if let Some(id_str) = rest.strip_suffix("/content") {
                    if let Ok(pane_id) = id_str.parse::<u64>() {
                        return Some(PaneQuery::GetPaneContent {
                            pane_id: Some(pane_id),
                            lines: 100,
                        });
                    }
                }
            }
            None
        }
    }
}

/// Build a `ResourceContent` from a query result.
pub fn build_resource_content(uri: &str, result: PaneQueryResult) -> ResourceContent {
    ResourceContent {
        uri: uri.to_string(),
        mime_type: Some(result.mime_type),
        text: Some(result.content),
        blob: None,
    }
}

// ---------------------------------------------------------------------------
// Default query fulfillment (when no pane channel is available)
// ---------------------------------------------------------------------------

/// Fulfill a git status query using the git CLI directly.
///
/// This is used as a fallback when the pane query channel is not connected.
pub fn fulfill_git_status() -> PaneQueryResult {
    let cwd = std::env::current_dir().unwrap_or_default();
    let git_ctx = crate::git_info::get_git_context(&cwd);

    let json = serde_json::json!({
        "branch": git_ctx.branch,
        "recent_commits": git_ctx.recent_commits,
        "staged_files": git_ctx.staged_files,
    });

    PaneQueryResult {
        content: serde_json::to_string_pretty(&json).unwrap_or_else(|_| "{}".to_string()),
        mime_type: "application/json".to_string(),
    }
}

/// Fulfill a command history query with an empty result (placeholder).
pub fn fulfill_command_history(_limit: usize) -> PaneQueryResult {
    let json = serde_json::json!({
        "commands": [],
        "note": "Command history not available (no pane connection)"
    });

    PaneQueryResult {
        content: serde_json::to_string_pretty(&json).unwrap_or_else(|_| "[]".to_string()),
        mime_type: "application/json".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_resources() {
        let resources = list_resources();
        assert_eq!(resources.len(), 5);

        let uris: Vec<&str> = resources.iter().map(|r| r.uri.as_str()).collect();
        assert!(uris.contains(&"terminal://pane/content"));
        assert!(uris.contains(&"terminal://pane/list"));
        assert!(uris.contains(&"elwood://session/log"));
        assert!(uris.contains(&"elwood://git/status"));
        assert!(uris.contains(&"elwood://commands/history"));
    }

    #[test]
    fn test_resolve_uri_known() {
        assert!(matches!(
            resolve_uri("terminal://pane/content"),
            Some(PaneQuery::GetPaneContent { pane_id: None, lines: 100 })
        ));
        assert!(matches!(
            resolve_uri("terminal://pane/list"),
            Some(PaneQuery::ListPanes)
        ));
        assert!(matches!(
            resolve_uri("elwood://session/log"),
            Some(PaneQuery::GetSessionLog)
        ));
        assert!(matches!(
            resolve_uri("elwood://git/status"),
            Some(PaneQuery::GetGitStatus)
        ));
        assert!(matches!(
            resolve_uri("elwood://commands/history"),
            Some(PaneQuery::GetCommandHistory { limit: 50 })
        ));
    }

    #[test]
    fn test_resolve_uri_parameterized_pane() {
        match resolve_uri("terminal://pane/42/content") {
            Some(PaneQuery::GetPaneContent {
                pane_id: Some(42),
                lines: 100,
            }) => {}
            other => panic!("expected GetPaneContent(42), got {other:?}"),
        }
    }

    #[test]
    fn test_resolve_uri_unknown() {
        assert!(resolve_uri("unknown://foo/bar").is_none());
        assert!(resolve_uri("").is_none());
        assert!(resolve_uri("terminal://pane/notanumber/content").is_none());
    }

    #[test]
    fn test_build_resource_content() {
        let result = PaneQueryResult {
            content: "hello world".to_string(),
            mime_type: "text/plain".to_string(),
        };
        let rc = build_resource_content("terminal://pane/content", result);
        assert_eq!(rc.uri, "terminal://pane/content");
        assert_eq!(rc.text.as_deref(), Some("hello world"));
        assert_eq!(rc.mime_type.as_deref(), Some("text/plain"));
        assert!(rc.blob.is_none());
    }

    #[test]
    fn test_fulfill_git_status() {
        let result = fulfill_git_status();
        assert_eq!(result.mime_type, "application/json");
        // Should be valid JSON
        let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert!(parsed.get("branch").is_some());
    }

    #[test]
    fn test_fulfill_command_history() {
        let result = fulfill_command_history(50);
        assert_eq!(result.mime_type, "application/json");
        let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert!(parsed.get("commands").is_some());
    }

    #[test]
    fn test_all_resources_have_descriptions() {
        for resource in list_resources() {
            assert!(
                resource.description.is_some(),
                "Resource {} missing description",
                resource.uri
            );
            assert!(
                resource.mime_type.is_some(),
                "Resource {} missing mime_type",
                resource.uri
            );
        }
    }
}
