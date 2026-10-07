// SPDX-License-Identifier: GPL-3.0-or-later
//! The connection to the DAW: connects lazily, and if no DAW is running
//! starts `libredaw --agent-request` and retries (17.1, mcp-8).

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use protocol::control::{Outcome, RequestBody, Transport};
use script::control::default_socket_path;
use script::{Client, ClientError};

/// What the agent is told when the DAW did not come up in time.
pub const WAITING_MESSAGE: &str = "waiting for the user in LibreDAW";

#[derive(Clone, Debug)]
pub struct Config {
    pub socket: PathBuf,
    /// Program started when no DAW is running.
    pub daw_command: OsString,
    /// How long to keep retrying after starting the DAW (30 s).
    pub startup_wait: Duration,
    pub retry_interval: Duration,
    /// Per-call deadline. A PRIVILEGED request waits up to 60 s for a human
    /// (17.1), so this is longer.
    pub call_timeout: Duration,
}

impl Config {
    pub fn new(socket: PathBuf) -> Config {
        Config {
            socket,
            daw_command: OsString::from("libredaw"),
            startup_wait: Duration::from_secs(30),
            retry_interval: Duration::from_millis(200),
            call_timeout: Duration::from_secs(75),
        }
    }

    /// `None` when `XDG_RUNTIME_DIR` is unset.
    pub fn from_env() -> Option<Config> {
        default_socket_path().map(Config::new)
    }
}

#[derive(Debug)]
pub enum ConnError {
    /// The DAW did not accept a connection in time.
    Waiting,
    /// The DAW program could not be started.
    CannotStart(String),
    /// The DAW is running but agent control is not enabled for this session.
    AgentsDisabled,
    /// The DAW refused the hello or spoke something else.
    Refused(String),
    /// The call failed after the request may have been sent.
    Lost(ClientError),
}

impl std::fmt::Display for ConnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnError::Waiting => write!(f, "{WAITING_MESSAGE}"),
            ConnError::CannotStart(s) => write!(
                f,
                "LibreDAW is not running and could not be started ({s}); ask the user to start LibreDAW"
            ),
            ConnError::AgentsDisabled => write!(
                f,
                "agent control is not enabled in LibreDAW; ask the user to enable agent control in LibreDAW (a banner or the preferences), then retry"
            ),
            ConnError::Refused(s) => write!(f, "LibreDAW refused the agent connection: {s}"),
            ConnError::Lost(ClientError::Timeout) => write!(
                f,
                "LibreDAW did not answer in time; the request may or may not have been applied, call project_get to check"
            ),
            ConnError::Lost(e) => write!(
                f,
                "connection to LibreDAW lost ({e}); the request may or may not have been applied, call project_get to check, then retry"
            ),
        }
    }
}

pub struct Conn {
    cfg: Config,
    client: Option<Client>,
    spawned: Option<Child>,
}

impl Conn {
    pub fn new(cfg: Config) -> Conn {
        Conn {
            cfg,
            client: None,
            spawned: None,
        }
    }

    pub fn call(
        &mut self,
        body: RequestBody,
        base_revision: Option<u64>,
    ) -> Result<Outcome, ConnError> {
        if self.client.as_ref().is_none_or(Client::is_broken) {
            self.client = Some(self.connect_or_start()?);
        }
        let client = self.client.as_mut().expect("just connected");
        match client.call(body, base_revision) {
            Ok(o) => Ok(o),
            Err(e) => {
                self.client = None;
                Err(ConnError::Lost(e))
            }
        }
    }

    fn connect_or_start(&mut self) -> Result<Client, ConnError> {
        let deadline = Instant::now() + self.cfg.startup_wait;
        let mut tried_start = false;
        let mut disabled = false;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let hello_timeout = left.max(Duration::from_secs(1));
            match Client::connect(
                &self.cfg.socket,
                Transport::Agent,
                "libredaw-mcp",
                hello_timeout,
            ) {
                Ok(mut c) => {
                    c.set_timeout(self.cfg.call_timeout);
                    return Ok(c);
                }
                Err(ClientError::Connect(_)) => {
                    if !tried_start && !self.daw_running() {
                        tried_start = true;
                        self.start_daw()?;
                    }
                }
                // The DAW is up but is still waiting for the user's
                // approval of this session: keep trying until the deadline.
                Err(ClientError::Timeout) => {}
                // Not enabled yet, or another session is being approved:
                // the user may be about to click, so keep trying.
                Err(ClientError::Refused(r)) if r == "agents_disabled" || r == "busy_owner" => {
                    disabled = r == "agents_disabled";
                }
                Err(e) => return Err(ConnError::Refused(e.to_string())),
            }
            if Instant::now() >= deadline {
                return Err(if disabled {
                    ConnError::AgentsDisabled
                } else {
                    ConnError::Waiting
                });
            }
            thread::sleep(self.cfg.retry_interval);
        }
    }

    fn daw_running(&mut self) -> bool {
        match &mut self.spawned {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    fn start_daw(&mut self) -> Result<(), ConnError> {
        let child = Command::new(&self.cfg.daw_command)
            .arg("--agent-request")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| ConnError::CannotStart(e.to_string()))?;
        self.spawned = Some(child);
        Ok(())
    }
}
