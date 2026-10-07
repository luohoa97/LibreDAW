// SPDX-License-Identifier: GPL-3.0-or-later
//! Runs one tool call: maps the plan to `RequestBody` values, sends them to
//! the UI through the ticket path (`Shared::submit`, answered by
//! `ControlServer::reply`) and waits for the outcomes.

use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};

use protocol::control::{ControlError, JobState, Outcome, ReplyBody, Request, RequestBody};
use protocol::edit::Edit;
use protocol::ids::PatternId;
use protocol::model::Project;
use serde_json::{Value, json};

use super::build;
use super::sanitize::{clean_text, for_agent};
use super::session::Session;
use super::summary;
use super::tools::{self, Plan, PlanError};
use crate::state::Shared;
use crate::suggest;

/// The result of a tool call as MCP wants it.
pub struct ToolOutput {
    pub is_error: bool,
    pub value: Value,
    /// Text for `content`; the JSON of `value` when `None`.
    pub text: Option<String>,
}

impl ToolOutput {
    pub fn ok(value: Value) -> ToolOutput {
        ToolOutput {
            is_error: false,
            value,
            text: None,
        }
    }

    pub fn error(code: &str, message: &str) -> ToolOutput {
        ToolOutput {
            is_error: true,
            value: json!({"error": {"code": code, "message": message}, "code": code, "message": message}),
            text: None,
        }
    }

    /// The human-readable message of an error.
    pub fn message(&self) -> String {
        self.value["message"]
            .as_str()
            .or_else(|| self.value["error"]["message"].as_str())
            .unwrap_or("unknown error")
            .to_string()
    }

    /// The MCP `tools/call` result.
    pub fn into_result(self) -> Value {
        let text = self.text.unwrap_or_else(|| self.value.to_string());
        json!({
            "content": [{"type": "text", "text": text}],
            "structuredContent": self.value,
            "isError": self.is_error,
        })
    }
}

enum CallError {
    Timeout,
    Closed,
}

/// Context of one tool call.
pub struct Exec {
    pub shared: Arc<Shared>,
    pub session: Arc<Session>,
    pub progress_token: Option<Value>,
}

impl Exec {
    pub fn new(shared: Arc<Shared>, session: Arc<Session>) -> Exec {
        Exec {
            shared,
            session,
            progress_token: None,
        }
    }

    fn call_timeout(&self) -> Duration {
        self.shared.approval_timeout + Duration::from_secs(15)
    }

    /// One control call. `Err` means the DAW did not answer.
    fn call(&self, body: RequestBody, base: Option<u64>) -> Result<Outcome, CallError> {
        let rid = self.session.next_request_id();
        let (tx, rx) = channel();
        self.session.expect_reply(rid, tx);
        let request = Request {
            id: rid,
            base_revision: base,
            body,
        };
        if let Err(outcome) = self.shared.submit(self.session.client_id(), request) {
            self.session.forget_reply(rid);
            return Ok(outcome);
        }
        match rx.recv_timeout(self.call_timeout()) {
            Ok(o) => Ok(o),
            Err(RecvTimeoutError::Timeout) => {
                self.session.forget_reply(rid);
                Err(CallError::Timeout)
            }
            Err(RecvTimeoutError::Disconnected) => Err(CallError::Closed),
        }
    }

    /// The current project and its revision.
    pub fn project(&self) -> Result<(u64, Arc<Project>), ToolOutput> {
        match self.call(RequestBody::ProjectGet, None) {
            Ok(Outcome::Ok {
                body: ReplyBody::Project { revision, project },
            }) => Ok((revision, project)),
            Ok(Outcome::Ok { .. }) => Err(ToolOutput::error(
                "internal",
                "unexpected reply to the project read",
            )),
            Ok(Outcome::Err { error }) => Err(control_error_out(&error)),
            Err(e) => Err(call_error_out(&e)),
        }
    }

    pub fn run(&self, plan: Plan) -> ToolOutput {
        match plan {
            Plan::Request {
                body,
                base_revision,
            } => self.send(body, base_revision, None),
            Plan::Job { body, wait } => self.job(body, wait),
            Plan::Steps(p) => {
                let length = match p.length {
                    Some(l) => l,
                    None => match self.project() {
                        Ok((_, project)) => match project.pattern(p.pattern) {
                            Some(pat) => pat.length_steps,
                            None => {
                                return ToolOutput::error(
                                    "not_found",
                                    &format!(
                                        "pattern {} does not exist; call project_summary for the ids",
                                        p.pattern
                                    ),
                                );
                            }
                        },
                        Err(o) => return o,
                    },
                };
                match tools::steps_edits(&p, length) {
                    Ok(edits) => self.send(RequestBody::Edit { edits }, None, None),
                    Err(m) => ToolOutput::error("bad_arguments", &m),
                }
            }
            Plan::Summary => match self.project() {
                Ok((rev, project)) => {
                    let text = summary::project_summary(&project, rev);
                    ToolOutput {
                        is_error: false,
                        value: json!({"revision": rev, "summary": text}),
                        text: Some(text),
                    }
                }
                Err(o) => o,
            },
            Plan::Grid(a) => {
                let (rev, project) = match self.project() {
                    Ok(p) => p,
                    Err(o) => return o,
                };
                match build::grid_edits(&project, a.pattern, &a.rows) {
                    Ok((edits, diff)) => self.apply(edits, diff, rev),
                    Err(m) => ToolOutput::error("bad_arguments", &m),
                }
            }
            Plan::GridGet(a) => self.grid_get(a.pattern),
            Plan::NotesWrite(a) => {
                let (rev, project) = match self.project() {
                    Ok(p) => p,
                    Err(o) => return o,
                };
                match build::notes_edits(&project, a.pattern, a.channel, &a.notes, a.replace) {
                    Ok((edits, diff)) => self.apply(edits, diff, rev),
                    Err(m) => ToolOutput::error("bad_arguments", &m),
                }
            }
            Plan::Suggestion(a) => {
                let (_, project) = match self.project() {
                    Ok(p) => p,
                    Err(o) => return o,
                };
                suggest::submit(&self.shared, a, &project)
            }
        }
    }

    /// Applies edits built from a fresh project read, with a custom diff.
    fn apply(&self, edits: Vec<Edit>, diff: Vec<String>, revision: u64) -> ToolOutput {
        if edits.is_empty() {
            return ToolOutput::ok(json!({
                "kind": "applied", "revision": revision, "created": [],
                "diff": ["no change: the project already matches"]
            }));
        }
        self.send(RequestBody::Edit { edits }, Some(revision), Some(diff))
    }

    fn grid_get(&self, pattern_id: PatternId) -> ToolOutput {
        let (rev, project) = match self.project() {
            Ok(p) => p,
            Err(o) => return o,
        };
        let Some(pattern) = project.pattern(pattern_id) else {
            return ToolOutput::error(
                "not_found",
                &format!("pattern {pattern_id} does not exist; call project_summary for the ids"),
            );
        };
        let bar = super::grid::GROUP;
        let mut rows = Vec::new();
        let mut text = format!(
            "pattern {pattern_id} {} {} steps, revision {rev} (grid: . off, x hit, X accent, digit ratchet)\n",
            summary::quoted(&pattern.name),
            pattern.length_steps
        );
        for ch in &project.channels {
            let existing = super::grid::read_existing(&project, pattern, ch.id);
            let (row, vel) = super::grid::existing_to_row(&existing);
            let grid = super::grid::format_grid(&row, Some(bar));
            text.push_str(&format!(
                "{} {} {grid}{}\n",
                ch.id,
                summary::quoted(&ch.name),
                if vel == super::grid::DEFAULT_VEL {
                    String::new()
                } else {
                    format!(" vel{vel}")
                }
            ));
            rows.push(json!({
                "channel": ch.id.0,
                "name": summary::quoted(&ch.name).trim_matches('"'),
                "grid": grid,
                "vel": vel,
            }));
        }
        ToolOutput {
            is_error: false,
            value: json!({"pattern": pattern_id.0, "steps": pattern.length_steps, "revision": rev, "rows": rows}),
            text: Some(text),
        }
    }

    /// One request to the UI, with the revision handling and the result
    /// shaping every tool shares.
    fn send(
        &self,
        body: RequestBody,
        base_override: Option<u64>,
        diff: Option<Vec<String>>,
    ) -> ToolOutput {
        let needs_revision = matches!(
            body,
            RequestBody::Edit { .. } | RequestBody::NotesList { .. } | RequestBody::KitAdd { .. }
        );
        let mut base = base_override;
        if needs_revision && base.is_none() {
            base = self.session.revision();
            if base.is_none() {
                // The agent never read this document: learn its revision.
                if let Err(o) = self.project() {
                    return o;
                }
                base = self.session.revision();
            }
        }
        let replaces = matches!(
            body,
            RequestBody::ProjectNew { .. } | RequestBody::ProjectOpen { .. }
        );
        let edits_for_diff = match &body {
            RequestBody::Edit { edits } => Some(edits.clone()),
            _ => None,
        };
        let result = self.call(body, if needs_revision { base } else { None });
        if replaces && matches!(result, Ok(Outcome::Ok { .. })) {
            // A new document: numbering restarts and every resource changed.
            self.session.set_revision(None);
            self.session.notify_project_resources();
        }
        match result {
            Ok(Outcome::Ok { body }) => {
                let mut v = serde_json::to_value(&body).unwrap_or(Value::Null);
                for_agent(&mut v);
                if matches!(body, ReplyBody::Applied(_)) {
                    let lines = match (diff, &edits_for_diff) {
                        (Some(d), _) => build::cap(d),
                        (None, Some(e)) => build::describe_edits(e),
                        (None, None) => Vec::new(),
                    };
                    v["diff"] = json!(lines);
                }
                ToolOutput::ok(v)
            }
            Ok(Outcome::Err { error }) => control_error_out(&error),
            Err(e) => call_error_out(&e),
        }
    }

    /// Export and analysis jobs: with `wait`, polls the job, sends MCP
    /// progress notifications, and returns the result.
    fn job(&self, body: RequestBody, wait: bool) -> ToolOutput {
        let started = self.send(body, None, None);
        if started.is_error || !wait {
            return started;
        }
        let Some(job) = started.value["job"].as_u64() else {
            return started;
        };
        let deadline = Instant::now() + self.shared.job_wait;
        let mut last = -1.0f64;
        loop {
            let (state, progress) = match self.call(RequestBody::JobStatus { job }, None) {
                Ok(Outcome::Ok {
                    body:
                        ReplyBody::JobStatus {
                            state, progress, ..
                        },
                }) => (state, progress),
                Ok(Outcome::Err { error }) => return control_error_out(&error),
                Ok(_) => return ToolOutput::error("internal", "unexpected reply to job_status"),
                Err(e) => return call_error_out(&e),
            };
            let pct = f64::from(progress) * 100.0;
            if pct > last {
                last = pct;
                if let Some(tok) = &self.progress_token {
                    self.session.progress(
                        tok,
                        pct,
                        &format!("job {job}: {state:?}").to_lowercase(),
                    );
                }
            }
            match state {
                JobState::Done => {
                    return match self.call(RequestBody::JobResult { job }, None) {
                        Ok(Outcome::Ok { body }) => {
                            let mut v = serde_json::to_value(&body).unwrap_or(Value::Null);
                            for_agent(&mut v);
                            v["job"] = json!(job);
                            ToolOutput::ok(v)
                        }
                        Ok(Outcome::Err { error }) => control_error_out(&error),
                        Err(e) => call_error_out(&e),
                    };
                }
                JobState::Failed | JobState::Cancelled => {
                    return ToolOutput::error(
                        "job_failed",
                        &format!("job {job} ended as {state:?}; call job_result for details"),
                    );
                }
                JobState::Queued | JobState::Running => {}
            }
            if Instant::now() >= deadline {
                return ToolOutput::ok(json!({
                    "job": job, "state": format!("{state:?}").to_lowercase(), "progress": progress,
                    "note": "still running; call job_status, then job_result when it is done"
                }));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Runs a `tools/call`.
pub fn call_tool(exec: &Exec, name: &str, args: Value) -> Result<ToolOutput, PlanError> {
    let plan = tools::plan(name, args)?;
    Ok(exec.run(plan))
}

fn call_error_out(e: &CallError) -> ToolOutput {
    match e {
        CallError::Timeout => ToolOutput::error(
            "timeout",
            "LibreDAW did not answer in time; the request may or may not have been applied, call project_summary to check",
        ),
        CallError::Closed => ToolOutput::error(
            "connection_lost",
            "the connection to LibreDAW closed; the request may or may not have been applied",
        ),
    }
}

/// The typed error from the DAW plus a sentence telling the agent what to do.
pub fn control_error_out(e: &ControlError) -> ToolOutput {
    let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
    for_agent(&mut v);
    let code = v["code"].as_str().unwrap_or("internal").to_string();
    let hint = match e {
        ControlError::Stale { current } => format!(
            "The document changed since you last read it (now revision {current}). Call project_summary, check the ids, and retry."
        ),
        ControlError::Busy => {
            "The user is in the middle of an edit gesture. Wait a few seconds and retry once.".into()
        }
        ControlError::NeedsUserApproval => "This action needs the user to click Approve in LibreDAW and nobody did within 60 s. Tell the user what you want to do and ask them to approve; do not retry in a loop.".into(),
        ControlError::Denied => "The user denied this action. Do not retry it; ask the user what to do instead.".into(),
        ControlError::NotAllowed => "This request is not allowed for agents.".into(),
        ControlError::Edit { index, error } => match index {
            Some(i) => format!(
                "Edit {i} (counting from 0) failed and nothing in the batch was applied: {}",
                clean_text(&error.to_string())
            ),
            None => format!(
                "The edit was rejected and nothing was changed: {}",
                clean_text(&error.to_string())
            ),
        },
        ControlError::NotFound { .. } => "That item does not exist. Call project_summary for current ids.".into(),
        ControlError::TooLarge { .. } => "The request is too large; split it into smaller batches.".into(),
        ControlError::BadRequest { reason } => format!("Bad request: {}", clean_text(reason)),
        ControlError::Internal { reason } => format!("LibreDAW internal error: {}", clean_text(reason)),
    };
    ToolOutput {
        is_error: true,
        value: json!({"error": v, "code": code, "message": hint}),
        text: None,
    }
}
