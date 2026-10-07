// SPDX-License-Identifier: GPL-3.0-or-later
//! `libredaw-mcp`: MCP server that lets agents control a running LibreDAW
//! through its control socket (SPEC 16, 17.1). Speaks MCP on stdin/stdout.

use std::io::{BufRead, Write};

use mcp::conn::Config;
use mcp::server::Server;

fn main() {
    let Some(config) = Config::from_env() else {
        eprintln!("libredaw-mcp: XDG_RUNTIME_DIR is not set; cannot find the LibreDAW socket");
        std::process::exit(1);
    };
    let mut server = Server::new(config);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = server.handle_line(&line) {
            if writeln!(stdout, "{reply}").is_err() || stdout.flush().is_err() {
                break;
            }
        }
    }
}
