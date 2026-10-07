// SPDX-License-Identifier: GPL-3.0-or-later
//! `libredaw-mcp setup`: registers this binary with the AI clients found on
//! the machine, after the user confirms each change; `--remove` undoes it.
//!
//! Clients with their own CLI (Claude Code, Codex) are configured through
//! it. For the others a JSON (or, for Codex without its CLI, TOML) config
//! file is edited, keeping everything else in it and leaving a backup.
//! All locations come from `SetupEnv`, so tests point it at a temporary
//! HOME and fake client binaries and never touch real configuration.

use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Map, Value, json};

/// The name the server is registered under in every client.
pub const SERVER_NAME: &str = "libredaw";

#[derive(Clone, Debug)]
pub struct SetupEnv {
    pub home: PathBuf,
    /// `$XDG_CONFIG_HOME` or `~/.config`.
    pub config_home: PathBuf,
    /// Directories searched for client programs, in order.
    pub path_dirs: Vec<PathBuf>,
    /// Absolute path of the `libredaw-mcp` binary to register.
    pub binary: PathBuf,
    /// Running inside the LibreDAW Flatpak sandbox: the AI clients are on
    /// the host, so setup prints the commands to run there and changes
    /// nothing (`flatpak_instructions`).
    pub flatpak: bool,
}

/// The Flatpak application id (decided by the owner).
pub const FLATPAK_APP_ID: &str = "io.github.luohoa97.LibreDAW";

/// The command AI clients on the host run to start the relay inside the
/// Flatpak.
pub const FLATPAK_COMMAND: &[&str] = &["flatpak", "run", "--command=libredaw-mcp", FLATPAK_APP_ID];

/// Whether this process runs inside a Flatpak sandbox (`FLATPAK_ID` is set,
/// or `/.flatpak-info` exists).
pub fn in_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some_and(|v| !v.is_empty())
        || Path::new("/.flatpak-info").exists()
}

impl SetupEnv {
    pub fn from_process() -> Result<SetupEnv, String> {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or("HOME is not set")?;
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let path_dirs = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        let binary = std::env::current_exe()
            .and_then(|p| p.canonicalize())
            .map_err(|e| format!("cannot find the path of this program: {e}"))?;
        Ok(SetupEnv {
            home,
            config_home,
            path_dirs,
            binary,
            flatpak: in_flatpak(),
        })
    }

    fn find_program(&self, name: &str) -> Option<PathBuf> {
        self.path_dirs
            .iter()
            .map(|d| d.join(name))
            .find(|p| p.is_file() && is_executable(p))
    }

    fn binary_str(&self) -> Result<&str, String> {
        self.binary
            .to_str()
            .ok_or_else(|| "the path of libredaw-mcp is not valid UTF-8".to_string())
    }
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientKind {
    ClaudeCode,
    Codex,
    Cursor,
    VsCode,
    GeminiCli,
    Zed,
}

pub const ALL_CLIENTS: [ClientKind; 6] = [
    ClientKind::ClaudeCode,
    ClientKind::Codex,
    ClientKind::Cursor,
    ClientKind::VsCode,
    ClientKind::GeminiCli,
    ClientKind::Zed,
];

impl ClientKind {
    pub fn slug(self) -> &'static str {
        match self {
            ClientKind::ClaudeCode => "claude-code",
            ClientKind::Codex => "codex",
            ClientKind::Cursor => "cursor",
            ClientKind::VsCode => "vscode",
            ClientKind::GeminiCli => "gemini",
            ClientKind::Zed => "zed",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            ClientKind::ClaudeCode => "Claude Code",
            ClientKind::Codex => "Codex",
            ClientKind::Cursor => "Cursor",
            ClientKind::VsCode => "VS Code",
            ClientKind::GeminiCli => "Gemini CLI",
            ClientKind::Zed => "Zed",
        }
    }
}

/// How one client is changed.
#[derive(Clone, Debug)]
pub enum Target {
    /// The client's own CLI.
    Cli {
        program: PathBuf,
        add: Vec<String>,
        remove: Vec<String>,
    },
    /// A JSON file with an object of servers under `servers_key`.
    Json {
        file: PathBuf,
        servers_key: &'static str,
        entry: Value,
    },
    /// Codex `config.toml`, edited as text.
    Toml { file: PathBuf, command: String },
}

#[derive(Clone, Debug)]
pub struct Detected {
    pub client: ClientKind,
    pub target: Target,
}

/// Finds the supported clients on this machine.
pub fn detect(env: &SetupEnv) -> Result<Vec<Detected>, String> {
    let bin = env.binary_str()?.to_string();
    let mut found = Vec::new();
    if let Some(program) = env.find_program("claude") {
        found.push(Detected {
            client: ClientKind::ClaudeCode,
            target: Target::Cli {
                program,
                add: strs(&["mcp", "add", "--scope", "user", SERVER_NAME, "--", &bin]),
                remove: strs(&["mcp", "remove", "--scope", "user", SERVER_NAME]),
            },
        });
    }
    if let Some(program) = env.find_program("codex") {
        found.push(Detected {
            client: ClientKind::Codex,
            target: Target::Cli {
                program,
                add: strs(&["mcp", "add", SERVER_NAME, "--", &bin]),
                remove: strs(&["mcp", "remove", SERVER_NAME]),
            },
        });
    } else if env.home.join(".codex").is_dir() {
        found.push(Detected {
            client: ClientKind::Codex,
            target: Target::Toml {
                file: env.home.join(".codex").join("config.toml"),
                command: bin.clone(),
            },
        });
    }
    if env.find_program("cursor").is_some() || env.home.join(".cursor").is_dir() {
        found.push(Detected {
            client: ClientKind::Cursor,
            target: Target::Json {
                file: env.home.join(".cursor").join("mcp.json"),
                servers_key: "mcpServers",
                entry: json!({"command": bin, "args": []}),
            },
        });
    }
    if env.find_program("code").is_some() || env.config_home.join("Code").is_dir() {
        found.push(Detected {
            client: ClientKind::VsCode,
            target: Target::Json {
                file: env.config_home.join("Code").join("User").join("mcp.json"),
                servers_key: "servers",
                entry: json!({"type": "stdio", "command": bin, "args": []}),
            },
        });
    }
    if env.find_program("gemini").is_some() || env.home.join(".gemini").is_dir() {
        found.push(Detected {
            client: ClientKind::GeminiCli,
            target: Target::Json {
                file: env.home.join(".gemini").join("settings.json"),
                servers_key: "mcpServers",
                entry: json!({"command": bin, "args": []}),
            },
        });
    }
    if env.find_program("zed").is_some() || env.config_home.join("zed").is_dir() {
        found.push(Detected {
            client: ClientKind::Zed,
            target: Target::Json {
                file: env.config_home.join("zed").join("settings.json"),
                servers_key: "context_servers",
                entry: json!({"command": bin, "args": [], "env": {}}),
            },
        });
    }
    Ok(found)
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub yes: bool,
    pub remove: bool,
    /// Client slugs; empty means every detected client.
    pub only: Vec<String>,
    pub help: bool,
}

pub const USAGE: &str = "usage: libredaw-mcp setup [--yes] [--remove] [--only <client>]...\n\
\n\
Registers libredaw-mcp with the AI clients found on this machine.\n\
Each change is shown first and applied only after you confirm (or with --yes).\n\
  --remove        undo: unregister libredaw-mcp instead\n\
  --only <name>   limit to one client: claude-code, codex, cursor, vscode, gemini, zed\n";

pub fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--yes" | "-y" => o.yes = true,
            "--remove" => o.remove = true,
            "--help" | "-h" => o.help = true,
            "--only" => {
                let v = it.next().ok_or("--only needs a client name")?;
                if !ALL_CLIENTS.iter().any(|c| c.slug() == v) {
                    return Err(format!("unknown client {v:?}"));
                }
                o.only.push(v.clone());
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(o)
}

/// Runs setup. Returns the process exit code: 0 when every confirmed
/// change succeeded (or nothing was found), 1 otherwise.
pub fn run(env: &SetupEnv, opts: &Options, input: &mut dyn BufRead, out: &mut dyn Write) -> i32 {
    if opts.help {
        let _ = write!(out, "{USAGE}");
        return 0;
    }
    if env.flatpak {
        return flatpak_instructions(opts, out);
    }
    let detected = match detect(env) {
        Ok(d) => d,
        Err(e) => {
            let _ = writeln!(out, "libredaw-mcp setup: {e}");
            return 1;
        }
    };
    let selected: Vec<&Detected> = detected
        .iter()
        .filter(|d| opts.only.is_empty() || opts.only.iter().any(|s| s == d.client.slug()))
        .collect();
    if selected.is_empty() {
        let _ = writeln!(
            out,
            "No supported AI client found (looked for Claude Code, Codex, Cursor, VS Code, Gemini CLI, Zed)."
        );
        return 0;
    }
    let verb = if opts.remove { "Remove" } else { "Add" };
    let mut failed = false;
    for d in selected {
        let _ = writeln!(out, "\n{}:", d.client.title());
        let _ = writeln!(out, "  {}", describe(&d.target, opts.remove));
        if !opts.yes && !confirm(input, out, verb) {
            let _ = writeln!(out, "  skipped");
            continue;
        }
        let result = if opts.remove {
            apply_remove(&d.target, env)
        } else {
            apply_add(&d.target, env)
        };
        match result {
            Ok(msg) => {
                let _ = writeln!(out, "  {msg}");
            }
            Err(e) => {
                failed = true;
                let _ = writeln!(out, "  FAILED: {e}");
            }
        }
    }
    i32::from(failed)
}

/// What the user runs on the host for one client, as text; `remove` gives
/// the undo.
fn host_step(client: ClientKind, remove: bool) -> String {
    let cmd = FLATPAK_COMMAND.join(" ");
    let args_json = json!(&FLATPAK_COMMAND[1..]);
    match (client, remove) {
        (ClientKind::ClaudeCode, false) => {
            format!("claude mcp add --scope user {SERVER_NAME} -- {cmd}")
        }
        (ClientKind::ClaudeCode, true) => format!("claude mcp remove --scope user {SERVER_NAME}"),
        (ClientKind::Codex, false) => format!("codex mcp add {SERVER_NAME} -- {cmd}"),
        (ClientKind::Codex, true) => format!("codex mcp remove {SERVER_NAME}"),
        (ClientKind::Cursor, false) => format!(
            "add to ~/.cursor/mcp.json: {{\"mcpServers\":{{\"{SERVER_NAME}\":{{\"command\":\"flatpak\",\"args\":{args_json}}}}}}}"
        ),
        (ClientKind::Cursor, true) => {
            format!("delete \"{SERVER_NAME}\" from \"mcpServers\" in ~/.cursor/mcp.json")
        }
        (ClientKind::VsCode, false) => format!(
            "code --add-mcp '{{\"name\":\"{SERVER_NAME}\",\"command\":\"flatpak\",\"args\":{args_json}}}'"
        ),
        (ClientKind::VsCode, true) => {
            format!("delete \"{SERVER_NAME}\" from \"servers\" in ~/.config/Code/User/mcp.json")
        }
        (ClientKind::GeminiCli, false) => format!("gemini mcp add -s user {SERVER_NAME} {cmd}"),
        (ClientKind::GeminiCli, true) => format!("gemini mcp remove -s user {SERVER_NAME}"),
        (ClientKind::Zed, false) => format!(
            "add to ~/.config/zed/settings.json: \"context_servers\": {{\"{SERVER_NAME}\": {{\"command\": \"flatpak\", \"args\": {args_json}, \"env\": {{}}}}}}"
        ),
        (ClientKind::Zed, true) => format!(
            "delete \"{SERVER_NAME}\" from \"context_servers\" in ~/.config/zed/settings.json"
        ),
    }
}

/// Inside the Flatpak sandbox: the AI clients (and their configs) live on
/// the host, which this sandbox cannot run programs on or write to, so
/// nothing is changed. Prints the exact steps for the user to run on the
/// host instead; the server entry is
/// `flatpak run --command=libredaw-mcp io.github.luohoa97.LibreDAW`.
fn flatpak_instructions(opts: &Options, out: &mut dyn Write) -> i32 {
    let _ = writeln!(
        out,
        "libredaw-mcp is running inside the LibreDAW Flatpak. AI clients live on your host system and this sandbox cannot run or configure them.\n\
Run these on the host (a terminal outside Flatpak); the server starts as `{}`:",
        FLATPAK_COMMAND.join(" ")
    );
    for c in ALL_CLIENTS {
        if !(opts.only.is_empty() || opts.only.iter().any(|s| s == c.slug())) {
            continue;
        }
        let _ = writeln!(out, "\n{}:\n  {}", c.title(), host_step(c, opts.remove));
    }
    let _ = writeln!(
        out,
        "\nNothing was changed. (`--yes` has no effect here: setup cannot reach the host.)"
    );
    0
}

fn confirm(input: &mut dyn BufRead, out: &mut dyn Write, verb: &str) -> bool {
    let _ = write!(out, "  {verb} this? [y/N] ");
    let _ = out.flush();
    let mut line = String::new();
    if input.read_line(&mut line).unwrap_or(0) == 0 {
        let _ = writeln!(out);
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn shell_word(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-=:@".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// The exact change, as shown to the user before it is applied.
pub fn describe(t: &Target, remove: bool) -> String {
    match t {
        Target::Cli {
            program,
            add,
            remove: rm,
        } => {
            let args = if remove { rm } else { add };
            let words: Vec<String> = std::iter::once(program.to_string_lossy().into_owned())
                .chain(args.iter().cloned())
                .map(|w| shell_word(&w))
                .collect();
            format!("run: {}", words.join(" "))
        }
        Target::Json {
            file,
            servers_key,
            entry,
        } => {
            if remove {
                format!(
                    "delete \"{SERVER_NAME}\" from \"{servers_key}\" in {}",
                    file.display()
                )
            } else {
                format!(
                    "set \"{servers_key}\".\"{SERVER_NAME}\" = {entry} in {}",
                    file.display()
                )
            }
        }
        Target::Toml { file, command } => {
            if remove {
                format!(
                    "delete the [mcp_servers.{SERVER_NAME}] table from {}",
                    file.display()
                )
            } else {
                format!(
                    "append to {}:\n{}",
                    file.display(),
                    toml_block(command).trim_end()
                )
            }
        }
    }
}

fn apply_add(t: &Target, env: &SetupEnv) -> Result<String, String> {
    match t {
        Target::Cli { program, add, .. } => run_cli(program, add, env),
        Target::Json {
            file,
            servers_key,
            entry,
        } => json_add(file, servers_key, entry),
        Target::Toml { file, command } => toml_add(file, command),
    }
}

fn apply_remove(t: &Target, env: &SetupEnv) -> Result<String, String> {
    match t {
        Target::Cli {
            program, remove, ..
        } => run_cli(program, remove, env),
        Target::Json {
            file, servers_key, ..
        } => json_remove(file, servers_key),
        Target::Toml { file, .. } => toml_remove(file),
    }
}

fn run_cli(program: &Path, args: &[String], env: &SetupEnv) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .env("HOME", &env.home)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", program.display()))?;
    if output.status.success() {
        return Ok("done".to_string());
    }
    let mut text = String::from_utf8_lossy(&output.stderr).into_owned();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(&output.stdout).into_owned();
    }
    let text: String = text.trim().chars().take(300).collect();
    Err(format!(
        "{} exited with {}: {text}",
        program.display(),
        output.status
    ))
}

// ---- file helpers -------------------------------------------------------

/// Copies an existing file to `<name>.libredaw-backup` (or `.1`, `.2`, ...)
/// without overwriting an earlier backup.
fn backup(file: &Path) -> Result<Option<PathBuf>, String> {
    if !file.exists() {
        return Ok(None);
    }
    let name = file
        .file_name()
        .ok_or("config path has no file name")?
        .to_string_lossy()
        .into_owned();
    let mut n = 0;
    let target = loop {
        let suffix = if n == 0 {
            ".libredaw-backup".to_string()
        } else {
            format!(".libredaw-backup.{n}")
        };
        let p = file.with_file_name(format!("{name}{suffix}"));
        if !p.exists() {
            break p;
        }
        n += 1;
    };
    fs::copy(file, &target)
        .map_err(|e| format!("cannot write backup {}: {e}", target.display()))?;
    Ok(Some(target))
}

/// Writes through a temporary file in the same directory, keeping the mode
/// of an existing file.
fn write_atomic(file: &Path, text: &str) -> Result<(), String> {
    let dir = file.parent().ok_or("config path has no directory")?;
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let tmp = file.with_extension(format!("libredaw-tmp-{}", std::process::id()));
    fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    if let Ok(meta) = fs::metadata(file) {
        let _ = fs::set_permissions(&tmp, meta.permissions());
    }
    fs::rename(&tmp, file).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot replace {}: {e}", file.display())
    })
}

fn read_optional(file: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(file) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", file.display())),
    }
}

fn parse_json_object(file: &Path, text: &str) -> Result<Map<String, Value>, String> {
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(format!(
            "{} is not a JSON object; not touching it. Add the entry by hand.",
            file.display()
        )),
        Err(e) => Err(format!(
            "{} is not plain JSON ({e}); it may contain comments. Not touching it. Add the entry by hand.",
            file.display()
        )),
    }
}

fn to_pretty(m: Map<String, Value>) -> String {
    let mut s = serde_json::to_string_pretty(&Value::Object(m)).unwrap_or_default();
    s.push('\n');
    s
}

fn json_add(file: &Path, key: &str, entry: &Value) -> Result<String, String> {
    let existing = read_optional(file)?;
    let mut root = parse_json_object(file, existing.as_deref().unwrap_or(""))?;
    let servers = root
        .entry(key.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(servers) = servers else {
        return Err(format!("\"{key}\" in {} is not an object", file.display()));
    };
    if servers.get(SERVER_NAME) == Some(entry) {
        return Ok("already set up, nothing to do".to_string());
    }
    let replaced = servers
        .insert(SERVER_NAME.to_string(), entry.clone())
        .is_some();
    let b = backup(file)?;
    write_atomic(file, &to_pretty(root))?;
    Ok(format!(
        "{} {}{}",
        if replaced { "updated" } else { "added to" },
        file.display(),
        b.map(|b| format!(" (backup: {})", b.display()))
            .unwrap_or_default()
    ))
}

fn json_remove(file: &Path, key: &str) -> Result<String, String> {
    let Some(text) = read_optional(file)? else {
        return Ok("nothing to remove (no config file)".to_string());
    };
    let mut root = parse_json_object(file, &text)?;
    let removed = match root.get_mut(key) {
        Some(Value::Object(servers)) => servers.shift_remove(SERVER_NAME).is_some(),
        _ => false,
    };
    if !removed {
        return Ok("nothing to remove".to_string());
    }
    let b = backup(file)?;
    write_atomic(file, &to_pretty(root))?;
    Ok(format!(
        "removed from {}{}",
        file.display(),
        b.map(|b| format!(" (backup: {})", b.display()))
            .unwrap_or_default()
    ))
}

fn toml_string(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            c if c.is_control() => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn toml_block(command: &str) -> String {
    format!(
        "[mcp_servers.{SERVER_NAME}]\ncommand = {}\nargs = []\n",
        toml_string(command)
    )
}

fn is_our_header(line: &str) -> bool {
    let l = line.trim();
    l == format!("[mcp_servers.{SERVER_NAME}]")
        || l.starts_with(&format!("[mcp_servers.{SERVER_NAME}."))
}

fn toml_add(file: &Path, command: &str) -> Result<String, String> {
    let existing = read_optional(file)?.unwrap_or_default();
    if existing.lines().any(is_our_header) {
        return Ok(
            "already has [mcp_servers.libredaw]; not changing it (use --remove first to replace)"
                .to_string(),
        );
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&toml_block(command));
    let b = backup(file)?;
    write_atomic(file, &text)?;
    Ok(format!(
        "added to {}{}",
        file.display(),
        b.map(|b| format!(" (backup: {})", b.display()))
            .unwrap_or_default()
    ))
}

fn toml_remove(file: &Path) -> Result<String, String> {
    let Some(text) = read_optional(file)? else {
        return Ok("nothing to remove (no config file)".to_string());
    };
    let mut kept = String::new();
    let mut skipping = false;
    let mut removed = false;
    for line in text.split_inclusive('\n') {
        let t = line.trim_start();
        if t.starts_with('[') {
            skipping = is_our_header(line);
            removed |= skipping;
        }
        if !skipping {
            kept.push_str(line);
        }
    }
    if !removed {
        return Ok("nothing to remove".to_string());
    }
    // Drop the blank separator line our append left behind.
    let kept = kept.trim_end_matches('\n').to_string();
    let kept = if kept.is_empty() { kept } else { kept + "\n" };
    let b = backup(file)?;
    write_atomic(file, &kept)?;
    Ok(format!(
        "removed from {}{}",
        file.display(),
        b.map(|b| format!(" (backup: {})", b.display()))
            .unwrap_or_default()
    ))
}
