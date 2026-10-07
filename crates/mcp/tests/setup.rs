// SPDX-License-Identifier: GPL-3.0-or-later
//! `libredaw-mcp setup` against temporary HOME directories and fake client
//! binaries. Nothing here reads or writes the real user configuration.

use std::fs;
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use mcp::setup::{Options, SetupEnv, parse_args, run};
use serde_json::{Value, json};

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct Sandbox {
    root: PathBuf,
    env: SetupEnv,
    log: PathBuf,
}

const BIN: &str = "/opt/libredaw/bin/libredaw-mcp";

fn sandbox() -> Sandbox {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!("ldaw-setup-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let home = root.join("home");
    let bindir = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&bindir).unwrap();
    Sandbox {
        env: SetupEnv {
            config_home: home.join(".config"),
            home,
            path_dirs: vec![bindir],
            binary: PathBuf::from(BIN),
        },
        log: root.join("calls.log"),
        root,
    }
}

impl Sandbox {
    /// A fake client program that appends its arguments and HOME to the log.
    fn fake(&self, name: &str, exit: i32) {
        let p = self.root.join("bin").join(name);
        // Write under a temporary name, close it, then rename into place so no
        // concurrent fork can hold a write fd on the executable (ETXTBSY).
        let tmp = self.root.join("bin").join(format!(".{name}.tmp"));
        fs::write(
            &tmp,
            format!(
                "#!/bin/sh\necho \"{name} $*\" >> '{}'\necho \"HOME=$HOME\" >> '{}'\nexit {exit}\n",
                self.log.display(),
                self.log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(&tmp, &p).unwrap();
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.starts_with("HOME="))
            .map(String::from)
            .collect()
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.env.home.join(rel)
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path(rel)).unwrap()
    }

    fn run(&self, args: &[&str], stdin: &str) -> (i32, String) {
        let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        let opts: Options = parse_args(&args).unwrap();
        // A fork in a parallel test thread can briefly leave the script open
        // for writing; retry the whole run on ETXTBSY (errno 26).
        let mut attempt = 0;
        loop {
            let mut out = Vec::new();
            let code = run(
                &self.env,
                &opts,
                &mut Cursor::new(stdin.to_string()),
                &mut out,
            );
            let text = String::from_utf8(out).unwrap();
            attempt += 1;
            if attempt < 10 && text.contains("Text file busy") {
                std::thread::sleep(std::time::Duration::from_millis(20));
                continue;
            }
            return (code, text);
        }
    }
}

fn backups(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("libredaw-backup"))
        .collect();
    v.sort();
    v
}

#[test]
fn nothing_installed_is_not_an_error() {
    let s = sandbox();
    let (code, out) = s.run(&["--yes"], "");
    assert_eq!(code, 0);
    assert!(out.contains("No supported AI client"), "{out}");
}

#[test]
fn claude_code_uses_its_cli_with_user_scope_and_absolute_path() {
    let s = sandbox();
    s.fake("claude", 0);
    let (code, out) = s.run(&[], "y\n");
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(&format!("claude mcp add --scope user libredaw -- {BIN}")),
        "the exact command is shown: {out}"
    );
    assert_eq!(
        s.calls(),
        vec![format!("claude mcp add --scope user libredaw -- {BIN}")]
    );
    let log = fs::read_to_string(&s.log).unwrap();
    assert!(log.contains(&format!("HOME={}", s.env.home.display())));

    let (code, _) = s.run(&["--remove", "--yes"], "");
    assert_eq!(code, 0);
    assert_eq!(s.calls()[1], "claude mcp remove --scope user libredaw");
}

#[test]
fn declining_or_no_input_changes_nothing() {
    let s = sandbox();
    s.fake("claude", 0);
    s.fake("codex", 0);
    let (code, out) = s.run(&[], "n\n");
    assert_eq!(code, 0);
    assert!(out.contains("skipped"));
    assert!(s.calls().is_empty(), "{:?}", s.calls());
    // End of input counts as no.
    s.run(&[], "");
    assert!(s.calls().is_empty());
}

#[test]
fn codex_cli_is_preferred_over_the_config_file() {
    let s = sandbox();
    s.fake("codex", 0);
    s.write(".codex/config.toml", "model = \"x\"\n");
    let (code, _) = s.run(&["--yes"], "");
    assert_eq!(code, 0);
    assert_eq!(s.calls(), vec![format!("codex mcp add libredaw -- {BIN}")]);
    assert_eq!(s.read(".codex/config.toml"), "model = \"x\"\n");
}

#[test]
fn a_failing_cli_is_reported_and_sets_the_exit_code() {
    let s = sandbox();
    s.fake("claude", 3);
    let (code, out) = s.run(&["--yes"], "");
    assert_eq!(code, 1);
    assert!(out.contains("FAILED"), "{out}");
}

#[test]
fn codex_toml_is_appended_and_removed_without_touching_the_rest() {
    let s = sandbox();
    let original = "model = \"o3\"\n\n[mcp_servers.other]\ncommand = \"x\"\n# keep me\n";
    s.write(".codex/config.toml", original);
    let (code, out) = s.run(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    let now = s.read(".codex/config.toml");
    assert!(now.starts_with(original));
    assert!(now.contains(&format!(
        "[mcp_servers.libredaw]\ncommand = \"{BIN}\"\nargs = []\n"
    )));
    assert_eq!(backups(&s.path(".codex")).len(), 1);
    assert_eq!(
        fs::read_to_string(s.path(".codex/config.toml.libredaw-backup")).unwrap(),
        original
    );
    // A second run does not duplicate it.
    s.run(&["--yes"], "");
    assert_eq!(s.read(".codex/config.toml").matches("libredaw]").count(), 1);
    // Remove restores the original text.
    let (code, _) = s.run(&["--remove", "--yes"], "");
    assert_eq!(code, 0);
    assert_eq!(s.read(".codex/config.toml"), original);
}

#[test]
fn codex_toml_is_created_when_missing() {
    let s = sandbox();
    fs::create_dir_all(s.path(".codex")).unwrap();
    s.run(&["--yes"], "");
    assert!(
        s.read(".codex/config.toml")
            .contains("[mcp_servers.libredaw]")
    );
    assert!(backups(&s.path(".codex")).is_empty(), "no file, no backup");
}

#[test]
fn cursor_json_keeps_other_servers_and_key_order() {
    let s = sandbox();
    let original = r#"{
  "zeta": 1,
  "mcpServers": {
    "zzz": {"command": "z"},
    "aaa": {"command": "a", "args": ["-v"]}
  },
  "alpha": [1, 2]
}"#;
    s.write(".cursor/mcp.json", original);
    let (code, out) = s.run(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    let v: Value = serde_json::from_str(&s.read(".cursor/mcp.json")).unwrap();
    assert_eq!(
        v["mcpServers"]["libredaw"],
        json!({"command": BIN, "args": []})
    );
    assert_eq!(v["mcpServers"]["zzz"], json!({"command": "z"}));
    assert_eq!(v["alpha"], json!([1, 2]));
    let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["zeta", "mcpServers", "alpha"]);
    let skeys: Vec<&String> = v["mcpServers"].as_object().unwrap().keys().collect();
    assert_eq!(skeys, ["zzz", "aaa", "libredaw"]);
    assert_eq!(
        fs::read_to_string(s.path(".cursor/mcp.json.libredaw-backup")).unwrap(),
        original
    );

    // Idempotent: no second backup, no change.
    let before = s.read(".cursor/mcp.json");
    let (_, out) = s.run(&["--yes"], "");
    assert!(out.contains("already set up"), "{out}");
    assert_eq!(s.read(".cursor/mcp.json"), before);
    assert_eq!(backups(&s.path(".cursor")).len(), 1);

    // Remove leaves everything else.
    let (code, _) = s.run(&["--remove", "--yes"], "");
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&s.read(".cursor/mcp.json")).unwrap();
    let skeys: Vec<&String> = v["mcpServers"].as_object().unwrap().keys().collect();
    assert_eq!(skeys, ["zzz", "aaa"]);
    assert_eq!(v["zeta"], 1);
}

#[test]
fn vscode_gemini_and_zed_get_their_own_formats() {
    let s = sandbox();
    s.write(".config/Code/User/settings.json", "{}");
    s.write(".gemini/settings.json", "{\"theme\": \"dark\"}");
    fs::create_dir_all(s.path(".config/zed")).unwrap();
    s.write(".config/zed/settings.json", "{\"vim_mode\": true}");
    let (code, out) = s.run(&["--yes"], "");
    assert_eq!(code, 0, "{out}");

    let vs: Value = serde_json::from_str(&s.read(".config/Code/User/mcp.json")).unwrap();
    assert_eq!(
        vs["servers"]["libredaw"],
        json!({"type": "stdio", "command": BIN, "args": []})
    );
    let g: Value = serde_json::from_str(&s.read(".gemini/settings.json")).unwrap();
    assert_eq!(g["theme"], "dark");
    assert_eq!(g["mcpServers"]["libredaw"]["command"], BIN);
    let z: Value = serde_json::from_str(&s.read(".config/zed/settings.json")).unwrap();
    assert_eq!(z["vim_mode"], true);
    assert_eq!(z["context_servers"]["libredaw"]["command"], BIN);
}

#[test]
fn json_with_comments_is_left_alone() {
    let s = sandbox();
    let original = "// my zed settings\n{\n  \"vim_mode\": true, // yes\n}\n";
    s.write(".config/zed/settings.json", original);
    let (code, out) = s.run(&["--yes"], "");
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("Add the entry by hand"), "{out}");
    assert_eq!(s.read(".config/zed/settings.json"), original);
    assert!(backups(&s.path(".config/zed")).is_empty());
}

#[test]
fn only_limits_the_clients() {
    let s = sandbox();
    s.fake("claude", 0);
    s.write(".cursor/mcp.json", "{}");
    let (code, _) = s.run(&["--yes", "--only", "cursor"], "");
    assert_eq!(code, 0);
    assert!(s.calls().is_empty());
    assert!(s.read(".cursor/mcp.json").contains("libredaw"));
}

#[test]
fn bad_arguments_are_rejected() {
    assert!(parse_args(&["--nope".to_string()]).is_err());
    assert!(parse_args(&["--only".to_string(), "emacs".to_string()]).is_err());
    assert!(parse_args(&["--only".to_string()]).is_err());
}

#[test]
fn remove_with_no_config_is_a_no_op() {
    let s = sandbox();
    fs::create_dir_all(s.path(".cursor")).unwrap();
    let (code, out) = s.run(&["--remove", "--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(!s.path(".cursor/mcp.json").exists());
}
