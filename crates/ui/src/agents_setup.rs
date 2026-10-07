// SPDX-License-Identifier: GPL-3.0-or-later
//! The one-line setup commands for AI clients (packaging/agents/README.md),
//! without GTK, so Preferences can offer a copy button for each.

use std::path::{Path, PathBuf};

pub struct AgentClient {
    pub name: &'static str,
    /// What the line is: a shell command, or text to add to a file.
    pub how: &'static str,
    pub line: String,
}

/// Quotes `s` for a POSIX shell when it needs it.
pub fn shell_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+:@%,=".contains(c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Escapes `s` for inside a JSON string.
fn json_escape(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if c.is_control() => {}
            c => o.push(c),
        }
    }
    o
}

/// Where `libredaw-mcp` is: next to this program if it is there, else the
/// bare name (found through `PATH`).
pub fn mcp_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("libredaw-mcp")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("libredaw-mcp"))
}

/// The lines for every client, for the given `libredaw-mcp` path.
pub fn clients(mcp: &Path) -> Vec<AgentClient> {
    let p = mcp.to_string_lossy().to_string();
    let q = shell_quote(&p);
    let j = json_escape(&p);
    vec![
        AgentClient {
            name: "Claude Code",
            how: "Run in a terminal",
            line: format!("claude mcp add --scope user libredaw -- {q}"),
        },
        AgentClient {
            name: "Codex",
            how: "Run in a terminal",
            line: format!("codex mcp add libredaw -- {q}"),
        },
        AgentClient {
            name: "Cursor",
            how: "Add to ~/.cursor/mcp.json",
            line: format!(
                "{{\"mcpServers\":{{\"libredaw\":{{\"command\":\"{j}\",\"args\":[]}}}}}}"
            ),
        },
        AgentClient {
            name: "VS Code",
            how: "Run in a terminal",
            line: format!(
                "code --add-mcp '{{\"name\":\"libredaw\",\"command\":\"{}\",\"args\":[]}}'",
                j.replace('\'', "'\\''")
            ),
        },
        AgentClient {
            name: "Gemini CLI",
            how: "Run in a terminal",
            line: format!("gemini mcp add -s user libredaw {q}"),
        },
        AgentClient {
            name: "Zed",
            how: "Add to ~/.config/zed/settings.json",
            line: format!(
                "\"context_servers\": {{\"libredaw\": {{\"command\": \"{j}\", \"args\": [], \"env\": {{}}}}}}"
            ),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_paths_stay_plain() {
        assert_eq!(
            shell_quote("/opt/libredaw/bin/libredaw-mcp"),
            "/opt/libredaw/bin/libredaw-mcp"
        );
        assert_eq!(shell_quote("libredaw-mcp"), "libredaw-mcp");
    }

    #[test]
    fn odd_paths_are_quoted() {
        assert_eq!(shell_quote("/my dir/mcp"), "'/my dir/mcp'");
        assert_eq!(shell_quote("/it's/mcp"), "'/it'\\''s/mcp'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a;rm -rf"), "'a;rm -rf'");
    }

    #[test]
    fn every_client_has_a_line_with_the_path() {
        let list = clients(Path::new("/x y/libredaw-mcp"));
        let names: Vec<_> = list.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            [
                "Claude Code",
                "Codex",
                "Cursor",
                "VS Code",
                "Gemini CLI",
                "Zed"
            ]
        );
        for c in &list {
            assert!(c.line.contains("/x y/libredaw-mcp"), "{}", c.name);
            assert!(!c.line.contains('\n'), "one line: {}", c.name);
        }
        assert!(
            list[0]
                .line
                .starts_with("claude mcp add --scope user libredaw -- '")
        );
    }

    #[test]
    fn json_lines_are_valid_json() {
        let list = clients(Path::new("/a\"b\\c/mcp"));
        let cursor = &list[2].line;
        assert!(cursor.contains("/a\\\"b\\\\c/mcp"), "{cursor}");
        assert!(cursor.starts_with('{') && cursor.ends_with('}'));
        assert_eq!(cursor.matches('{').count(), cursor.matches('}').count());
    }
}
