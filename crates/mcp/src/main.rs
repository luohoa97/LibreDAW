// SPDX-License-Identifier: GPL-3.0-or-later
//! `libredaw-mcp`: MCP server that lets agents control a running LibreDAW
//! through its control socket (SPEC 16, 17.1). Speaks MCP on stdin/stdout.
//! `libredaw-mcp setup` registers it with installed AI clients instead.

use std::io::{BufRead, Write};

use mcp::conn::Config;
use mcp::server::Server;
use mcp::setup;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("setup") {
        std::process::exit(run_setup(&args[1..]));
    }
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
        if let Some(reply) = server.handle_line(&line)
            && (writeln!(stdout, "{reply}").is_err() || stdout.flush().is_err())
        {
            break;
        }
    }
}

fn run_setup(args: &[String]) -> i32 {
    let opts = match setup::parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("libredaw-mcp setup: {e}\n\n{}", setup::USAGE);
            return 2;
        }
    };
    let env = match setup::SetupEnv::from_process() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("libredaw-mcp setup: {e}");
            return 1;
        }
    };
    let stdin = std::io::stdin();
    setup::run(
        &env,
        &opts,
        &mut stdin.lock(),
        &mut std::io::stdout().lock(),
    )
}
