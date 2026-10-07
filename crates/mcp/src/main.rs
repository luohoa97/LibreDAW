// SPDX-License-Identifier: GPL-3.0-or-later
//! `libredaw-mcp`: relays an AI client's MCP stdio to the running
//! LibreDAW's control socket (SPEC 16, 18.3), starting LibreDAW if needed.
//! `libredaw-mcp setup` registers it with installed AI clients instead.

use mcp::relay::{self, Config};
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
    let stdin = std::io::stdin();
    if let Err(e) = relay::run(config, stdin.lock(), std::io::stdout()) {
        eprintln!("libredaw-mcp: {e}");
        std::process::exit(1);
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
