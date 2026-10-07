// SPDX-License-Identifier: GPL-3.0-or-later
//! Runs one tool call: maps the plan to `RequestBody` values, sends them to
//! the UI through the ticket path (`Shared::submit`, answered by
//! `ControlServer::reply`) and waits for the outcomes. The UI answers them
//! with the same session operations its own widgets use (`BRIDGE.md`).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};

use protocol::control::{
    BranchInfo, ControlError, JobState, Outcome, ReplyBody, Request, RequestBody,
};
use protocol::edit::{Applied, EditError};
use protocol::model::{Project, ticks_per_bar};
use protocol::validate::ValidationError;
use serde_json::{Value, json};

use super::build::{self, Target};
use super::compose::{self, Built};
use super::ids::{self, IdGen};
use super::notes;
use super::sanitize::{clean_text, for_agent};
use super::session::Session;
use super::summary::{self, plain};
use super::tools::{self, Compose, InspectArgs, JobArgs, JobKind, Plan, PlanError};
use crate::state::Shared;
use crate::suggest;

/// Commit ids shown to the agent: enough to be unique (the DAW accepts a
/// prefix of 8 or more characters, and provisional ids are 16).
const COMMIT_CHARS: usize = 16;
/// Most notes listed with ids per content in `content_get`.
const NOTES_LISTED: usize = 256;

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

type Out<T> = Result<T, ToolOutput>;

fn short(commit: &str) -> String {
    commit.chars().take(COMMIT_CHARS).collect()
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

    /// A call whose error is already a tool output.
    fn body(&self, body: RequestBody, base: Option<u64>) -> Out<ReplyBody> {
        match self.call(body, base) {
            Ok(Outcome::Ok { body }) => Ok(body),
            Ok(Outcome::Err { error }) => Err(control_error_out(&error)),
            Err(e) => Err(call_error_out(&e)),
        }
    }

    /// The current project and its revision.
    pub fn project(&self) -> Out<(u64, Arc<Project>)> {
        match self.body(RequestBody::ProjectGet, None)? {
            ReplyBody::Project {
                revision, project, ..
            } => Ok((revision, project)),
            _ => Err(unexpected("the project read")),
        }
    }

    /// Learns the revision after a request that changed the document but
    /// did not say so.
    fn refresh_revision(&self) -> Option<u64> {
        self.project().ok().map(|(r, _)| r)
    }

    fn branches(&self) -> Out<(String, Vec<BranchInfo>)> {
        match self.body(RequestBody::BranchList, None)? {
            ReplyBody::Branches { current, branches } => Ok((current, branches)),
            _ => Err(unexpected("the branch list")),
        }
    }

    fn head(&self) -> Out<String> {
        match self.body(
            RequestBody::HistoryTree {
                since: None,
                limit: 1,
            },
            None,
        )? {
            ReplyBody::HistoryTree { head, .. } => Ok(head),
            _ => Err(unexpected("the history read")),
        }
    }

    pub fn run(&self, plan: Plan) -> ToolOutput {
        let r = match plan {
            Plan::Request {
                body,
                base_revision,
                refresh,
            } => return self.send(body, base_revision, refresh),
            Plan::Compose(c) => self.compose(&c),
            Plan::Summary => self.summary(),
            Plan::Inspect(a) => self.inspect(&a),
            Plan::ContentGet(t) => self.content_get(&t),
            Plan::Transport => self.transport(),
            Plan::Seek(at) => self.seek(&at),
            Plan::Job(a) => self.job(&a),
            Plan::JobQuery { job, cancel } => self.job_query(job, cancel),
            Plan::Undo { redo, steps } => self.undo(redo, steps),
            Plan::History { since, limit } => self.history(since, limit),
            Plan::HistoryDiff { from, to } => self.history_diff(from, to),
            Plan::VersionSave { name } => self.version_save(name),
            Plan::BranchCreate { name, from } => self.branch_create(name, from),
            Plan::BranchSet {
                branch,
                name,
                archive,
            } => self.branch_set(branch, name, archive),
            Plan::Plugins { scan } => self.plugins(scan),
            Plan::Suggestion(a) => self
                .project()
                .map(|(_, project)| suggest::submit(&self.shared, a, &project)),
        };
        r.unwrap_or_else(|e| e)
    }

    // ---- editing ----------------------------------------------------------

    /// Reads the project, builds one batch, sends it with that revision.
    fn compose(&self, c: &Compose) -> Out<ToolOutput> {
        // A second attempt only when the predicted ids were wrong (the user
        // undid the creation of something, so the counter is higher).
        for attempt in 0..2 {
            let (rev, project) = self.project()?;
            let mut ids = IdGen::new(&project, self.session.id_floor());
            let built = c
                .build(&project, &mut ids)
                .map_err(|m| ToolOutput::error("bad_arguments", &clean_text(&m)))?;
            if built.edits.is_empty() {
                return Ok(ToolOutput::ok(json!({
                    "kind": "applied", "revision": rev, "created": [],
                    "diff": ["no change: the project already matches"]
                })));
            }
            let body = RequestBody::Edit {
                edits: built.edits.clone(),
            };
            let applied = match self.call(body, Some(rev)) {
                Ok(Outcome::Ok {
                    body: ReplyBody::Applied(a),
                }) => a,
                Ok(Outcome::Ok { .. }) => return Err(unexpected("the edit")),
                Ok(Outcome::Err { error }) => {
                    return Err(edit_error_out(&error, &built.labels));
                }
                Err(e) => return Err(call_error_out(&e)),
            };
            if ids::matches(&built.predicted, &applied.created) {
                return Ok(self.composed_reply(applied, built));
            }
            // Take our own commit back; `deliver` raised the id floor.
            self.body(RequestBody::Undo, None)?;
            if attempt == 1 {
                return Err(ToolOutput::error(
                    "internal",
                    "LibreDAW numbered the new objects differently than expected; the change was undone. Call project_summary and try again.",
                ));
            }
        }
        unreachable!("the loop returns")
    }

    fn composed_reply(&self, applied: Applied, built: Built) -> ToolOutput {
        let mut v = json!({
            "kind": "applied",
            "revision": applied.revision,
            "created": applied.created,
            "diff": build::cap(built.diff),
        });
        for (k, val) in built.reply {
            v[k] = val;
        }
        let touched: BTreeSet<u32> = built
            .report_clips
            .iter()
            .map(|c| c.0)
            .chain(applied.created.iter().copied())
            .collect();
        if !touched.is_empty()
            && let Ok((_, project)) = self.project()
        {
            let tpb = ticks_per_bar(project.time_sig_num);
            let clips: Vec<Value> = project
                .clips
                .iter()
                .filter(|c| touched.contains(&c.id.0))
                .map(|c| {
                    json!({"clip": c.id.0, "instrument": c.instrument.0, "content": c.pattern.0,
                       "start": notes::fraction(c.start, tpb), "length": notes::fraction(c.len, tpb), "muted": c.muted})
                })
                .collect();
            if !clips.is_empty() {
                v["clips"] = json!(clips);
            }
        }
        for_agent(&mut v);
        ToolOutput::ok(v)
    }

    /// One request to the UI, with the revision handling and the result
    /// shaping every simple tool shares.
    fn send(&self, body: RequestBody, base_override: Option<u64>, refresh: bool) -> ToolOutput {
        let needs_revision = matches!(
            body,
            RequestBody::Edit { .. }
                | RequestBody::KitAdd { .. }
                | RequestBody::SoundAdd { .. }
                | RequestBody::BranchSwitch { .. }
                | RequestBody::VersionRestore { .. }
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
        let page = match &body {
            RequestBody::SoundSearch { offset, .. } => Some(*offset as usize),
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
                if matches!(body, ReplyBody::Applied(_)) {
                    v["diff"] = json!(
                        edits_for_diff
                            .as_deref()
                            .map(build::describe_edits)
                            .unwrap_or_default()
                    );
                }
                if refresh && let Some(r) = self.refresh_revision() {
                    v["revision"] = json!(r);
                }
                if let ReplyBody::Branches { current, branches } = &body {
                    v = branches_json(current, branches);
                }
                if let (
                    ReplyBody::Sounds {
                        sounds,
                        total,
                        notes,
                    },
                    Some(offset),
                ) = (&body, page)
                {
                    v = sounds_json(sounds, *total, notes, offset);
                }
                for_agent(&mut v);
                ToolOutput::ok(v)
            }
            Ok(Outcome::Err { error }) => control_error_out(&error),
            Err(e) => call_error_out(&e),
        }
    }

    // ---- reading ------------------------------------------------------------

    fn summary(&self) -> Out<ToolOutput> {
        let (rev, project) = self.project()?;
        let branch = self.branches().ok().and_then(|(cur, list)| {
            list.into_iter()
                .find(|b| b.branch == cur)
                .map(|b| b.name)
                .or(Some(cur))
        });
        let text = summary::project_summary(&project, rev, branch.as_deref());
        Ok(ToolOutput {
            is_error: false,
            value: json!({"revision": rev, "summary": text}),
            text: Some(text),
        })
    }

    fn inspect(&self, a: &InspectArgs) -> Out<ToolOutput> {
        let (rev, project) = self.project()?;
        let missing = |what: &str, id: u32| {
            ToolOutput::error(
                "not_found",
                &format!("{what} {id} does not exist; call project_summary for the ids"),
            )
        };
        let mut v = if let Some(id) = a.instrument {
            let c = project
                .channel(id)
                .ok_or_else(|| missing("instrument", id.0))?;
            let clips: Vec<u32> = project
                .clips
                .iter()
                .filter(|x| x.instrument == id)
                .map(|x| x.id.0)
                .collect();
            let contents: Vec<u32> = project
                .patterns
                .iter()
                .filter(|x| x.instrument == id)
                .map(|x| x.id.0)
                .collect();
            json!({"instrument": c, "clips": clips, "contents": contents})
        } else if let Some(id) = a.track {
            let t = project
                .track(id)
                .ok_or_else(|| missing("mixer track", id.0))?;
            let feeds: Vec<u32> = project
                .channels
                .iter()
                .filter(|c| c.track == id)
                .map(|c| c.id.0)
                .collect();
            json!({"track": t, "instruments": feeds})
        } else if let Some(id) = a.clip {
            let c = project
                .clips
                .iter()
                .find(|x| x.id == id)
                .ok_or_else(|| missing("clip", id.0))?;
            let linked: Vec<u32> = project
                .clips
                .iter()
                .filter(|x| x.pattern == c.pattern && x.id != id)
                .map(|x| x.id.0)
                .collect();
            json!({"clip": c, "linked_clips": linked})
        } else if let Some(id) = a.content {
            let p = project
                .pattern(id)
                .ok_or_else(|| missing("content", id.0))?;
            json!({"content": p})
        } else if let Some(id) = a.insert {
            let (track, ins) = project
                .tracks
                .iter()
                .find_map(|t| {
                    t.inserts
                        .iter()
                        .find(|i| i.instance() == id)
                        .map(|i| (t.id, i))
                })
                .ok_or_else(|| missing("insert", id.0))?;
            json!({"track": track.0, "insert": ins})
        } else {
            return Err(ToolOutput::error("bad_arguments", "name one object"));
        };
        v["revision"] = json!(rev);
        v["units"] = json!("times in this JSON are ticks: 960 per beat, 3840 per 4/4 bar");
        for_agent(&mut v);
        Ok(ToolOutput::ok(v))
    }

    fn content_get(&self, targets: &[Target]) -> Out<ToolOutput> {
        let (rev, project) = self.project()?;
        let ids = if targets.is_empty() {
            project.patterns.iter().map(|p| p.id).collect()
        } else {
            targets
                .iter()
                .map(|t| build::resolve(&project, *t))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|m| ToolOutput::error("not_found", &m))?
        };
        let tpb = ticks_per_bar(project.time_sig_num);
        let mut text = format!("LibreDAW r{rev} (text in \"quotes\" is project data)\n");
        let mut items = Vec::new();
        for id in ids {
            let Some(p) = project.pattern(id) else {
                continue;
            };
            let (grid, vel) = summary::grid_text(&project, p);
            let view = summary::content_view(&project, p);
            let listed: Vec<Value> = view
                .other
                .iter()
                .take(NOTES_LISTED)
                .map(|n| json!({"id": n.id.0, "note": notes::format_notes(std::slice::from_ref(n), tpb)}))
                .collect();
            let clips: Vec<u32> = project
                .clips
                .iter()
                .filter(|c| c.pattern == id)
                .map(|c| c.id.0)
                .collect();
            let notes_text = notes::format_notes(&view.other, tpb);
            text.push_str(&format!(
                "P{id} {} I{} {} steps: steps {grid}{} | notes {}\n",
                summary::quoted(&p.name),
                p.instrument,
                p.length_steps,
                if vel == super::grid::DEFAULT_VEL {
                    String::new()
                } else {
                    format!(" vel{vel}")
                },
                if notes_text.is_empty() {
                    "(none)".to_string()
                } else {
                    notes_text.clone()
                }
            ));
            items.push(json!({
                "content": id.0, "name": plain(&p.name), "instrument": p.instrument.0,
                "steps": p.length_steps, "step_length": notes::fraction(p.step_ticks, tpb), "swing": p.swing,
                "grid": grid, "grid_vel": vel, "notes_text": notes_text, "notes": listed,
                "notes_total": view.other.len(), "clips": clips,
            }));
        }
        Ok(ToolOutput {
            is_error: false,
            value: json!({"revision": rev, "contents": items}),
            text: Some(text),
        })
    }

    fn seek(&self, at: &Value) -> Out<ToolOutput> {
        let (_, project) = self.project()?;
        let tpb = ticks_per_bar(project.time_sig_num);
        let tick = match compose::ticks_of(at, tpb, "position") {
            Ok(t) => t,
            Err(m) => return Ok(ToolOutput::error("bad_arguments", &m)),
        };
        self.body(
            RequestBody::Seek {
                tick: u64::from(tick),
            },
            None,
        )?;
        Ok(ToolOutput::ok(
            json!({"position": notes::fraction(tick, tpb), "tick": tick}),
        ))
    }

    fn transport(&self) -> Out<ToolOutput> {
        let ReplyBody::Transport {
            playing,
            tick,
            tempo_bpm,
            loop_region,
        } = self.body(RequestBody::TransportState, None)?
        else {
            return Err(unexpected("the transport read"));
        };
        let tpb = self
            .project()
            .map(|(_, p)| ticks_per_bar(p.time_sig_num))
            .unwrap_or(3840);
        let pos = u32::try_from(tick).unwrap_or(u32::MAX);
        Ok(ToolOutput::ok(json!({
            "playing": playing,
            "position": notes::fraction(pos - pos % 240, tpb),
            "tick": tick,
            "tempo_bpm": tempo_bpm,
            "loop": {"start": notes::fraction(loop_region.start, tpb), "end": notes::fraction(loop_region.end, tpb), "enabled": loop_region.enabled},
        })))
    }

    // ---- jobs -----------------------------------------------------------------

    /// Export and analysis jobs: with `wait`, polls the job, sends MCP
    /// progress notifications, and returns the result.
    fn job(&self, a: &JobArgs) -> Out<ToolOutput> {
        let (start, end) = if a.start.is_some() || a.end.is_some() {
            let (_, project) = self.project()?;
            let tpb = ticks_per_bar(project.time_sig_num);
            let conv = |v: &Option<Value>, what: &str| -> Out<Option<u32>> {
                v.as_ref()
                    .map(|v| {
                        let s = match v {
                            Value::Number(n) => n.to_string(),
                            Value::String(s) => s.clone(),
                            _ => String::new(),
                        };
                        notes::parse_ticks(&s, tpb, what)
                            .map_err(|m| ToolOutput::error("bad_arguments", &clean_text(&m)))
                    })
                    .transpose()
            };
            (conv(&a.start, "start")?, conv(&a.end, "end")?)
        } else {
            (None, None)
        };
        let body = match a.kind {
            JobKind::Export(format) => RequestBody::ExportWav {
                format,
                start,
                end,
                tail_seconds: a.tail_seconds,
            },
            JobKind::Analyze => RequestBody::Analyze { start, end },
        };
        let ReplyBody::Job { job, revision } = self.body(body, None)? else {
            return Err(unexpected("the job start"));
        };
        if !a.wait {
            return Ok(ToolOutput::ok(
                json!({"job": job, "revision": revision, "note": "call job to see its state and result"}),
            ));
        }
        let deadline = Instant::now() + self.shared.job_wait;
        let mut last = -1.0f64;
        loop {
            let ReplyBody::JobStatus {
                state, progress, ..
            } = self.body(RequestBody::JobStatus { job }, None)?
            else {
                return Err(unexpected("job status"));
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
                JobState::Done => return self.job_result(job),
                JobState::Failed | JobState::Cancelled => {
                    return Err(ToolOutput::error(
                        "job_failed",
                        &format!(
                            "job {job} ended as {}; call job for details",
                            format!("{state:?}").to_lowercase()
                        ),
                    ));
                }
                JobState::Queued | JobState::Running => {}
            }
            if Instant::now() >= deadline {
                return Ok(ToolOutput::ok(json!({
                    "job": job, "state": format!("{state:?}").to_lowercase(), "progress": progress,
                    "note": "still running; call job again later"
                })));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn job_result(&self, job: u64) -> Out<ToolOutput> {
        let body = self.body(RequestBody::JobResult { job }, None)?;
        let mut v = serde_json::to_value(&body).unwrap_or(Value::Null);
        if let ReplyBody::Analysis(a) = &body {
            v["bars"] = json!(bar_table(&a.bars));
        }
        for_agent(&mut v);
        v["job"] = json!(job);
        v["state"] = json!("done");
        Ok(ToolOutput::ok(v))
    }

    fn job_query(&self, job: u64, cancel: bool) -> Out<ToolOutput> {
        if cancel {
            self.body(RequestBody::JobCancel { job }, None)?;
            return Ok(ToolOutput::ok(json!({"job": job, "state": "cancelled"})));
        }
        let ReplyBody::JobStatus {
            state, progress, ..
        } = self.body(RequestBody::JobStatus { job }, None)?
        else {
            return Err(unexpected("job status"));
        };
        if state == JobState::Done {
            return self.job_result(job);
        }
        Ok(ToolOutput::ok(
            json!({"job": job, "state": format!("{state:?}").to_lowercase(), "progress": progress}),
        ))
    }

    // ---- history ----------------------------------------------------------------

    fn undo(&self, redo: bool, steps: u32) -> Out<ToolOutput> {
        let mut done = 0;
        let mut last_err = None;
        for _ in 0..steps {
            let body = if redo {
                RequestBody::Redo
            } else {
                RequestBody::Undo
            };
            match self.body(body, None) {
                Ok(_) => done += 1,
                Err(e) => {
                    last_err = Some(e);
                    break;
                }
            }
        }
        if done == 0 {
            let e = last_err.expect("one attempt");
            let msg = e.message();
            return Err(ToolOutput::error(
                "nothing_to_undo",
                &format!(
                    "{}: {msg}. Only your own changes on the current branch can be {}.",
                    if redo {
                        "Nothing to redo"
                    } else {
                        "Nothing to undo"
                    },
                    if redo { "redone" } else { "undone" }
                ),
            ));
        }
        let rev = self.refresh_revision();
        let key = if redo { "redone" } else { "undone" };
        Ok(ToolOutput::ok(
            json!({key: done, "revision": rev, "note": if done < steps { "stopped early: no more of your own changes" } else { "" }}),
        ))
    }

    fn history(&self, since: Option<String>, limit: u32) -> Out<ToolOutput> {
        let ReplyBody::HistoryTree { head, nodes } =
            self.body(RequestBody::HistoryTree { since, limit }, None)?
        else {
            return Err(unexpected("the history read"));
        };
        let nodes: Vec<Value> = nodes
            .iter()
            .map(|n| {
                let mut v = json!({"commit": short(&n.commit), "branch": n.branch, "author": n.author, "description": n.description});
                if let Some(p) = &n.parent {
                    v["parent"] = json!(short(p));
                }
                if let Some(name) = &n.name {
                    v["name"] = json!(name);
                }
                v
            })
            .collect();
        let mut v = json!({"head": short(&head), "nodes": nodes});
        for_agent(&mut v);
        Ok(ToolOutput::ok(v))
    }

    fn history_diff(&self, from: String, to: Option<String>) -> Out<ToolOutput> {
        let to = match to {
            Some(t) => t,
            None => self.head()?,
        };
        let ReplyBody::HistoryDiff { lines } = self.body(
            RequestBody::HistoryDiff {
                from: from.clone(),
                to: to.clone(),
            },
            None,
        )?
        else {
            return Err(unexpected("the diff"));
        };
        let mut v = json!({"from": short(&from), "to": short(&to), "lines": build::cap(lines)});
        for_agent(&mut v);
        Ok(ToolOutput::ok(v))
    }

    fn version_save(&self, name: String) -> Out<ToolOutput> {
        self.body(RequestBody::VersionSave { name: name.clone() }, None)?;
        let head = self.head()?;
        let mut v = json!({"name": name, "commit": short(&head)});
        for_agent(&mut v);
        Ok(ToolOutput::ok(v))
    }

    fn branch_create(&self, name: String, from: Option<String>) -> Out<ToolOutput> {
        let (previous, list) = self.branches()?;
        // `from` may name a branch: start from its head.
        let from = from.map(|f| {
            list.iter()
                .find(|b| b.branch == f || b.name == f)
                .map_or(f, |b| b.head.clone())
        });
        let reply = self.body(RequestBody::BranchCreate { name, from }, None)?;
        let (current, list) = match reply {
            ReplyBody::Branches { current, branches } => (current, branches),
            _ => self.branches()?,
        };
        let rev = self.refresh_revision();
        let b = list
            .iter()
            .find(|b| b.branch == current)
            .ok_or_else(|| unexpected("the new branch"))?;
        let mut v = json!({
            "branch": b.branch, "name": b.name, "base": short(&b.base), "head": short(&b.head),
            "previous_branch": previous, "revision": rev,
            "note": "You are on the new branch now; edits go there. Make the next version with branch_create from this `base`; go back with branch_switch.",
        });
        for_agent(&mut v);
        Ok(ToolOutput::ok(v))
    }

    fn branch_set(&self, branch: String, name: Option<String>, archive: bool) -> Out<ToolOutput> {
        if let Some(n) = name {
            self.body(
                RequestBody::BranchRename {
                    branch: branch.clone(),
                    name: n,
                },
                None,
            )?;
        }
        if archive {
            self.body(
                RequestBody::BranchArchive {
                    branch: branch.clone(),
                },
                None,
            )?;
        }
        let (current, list) = self.branches()?;
        let mut v = branches_json(&current, &list);
        for_agent(&mut v);
        Ok(ToolOutput::ok(v))
    }

    fn plugins(&self, scan: bool) -> Out<ToolOutput> {
        if scan {
            self.body(RequestBody::PluginScan, None)?;
        }
        let body = self.body(RequestBody::PluginList, None)?;
        let mut v = serde_json::to_value(&body).unwrap_or(Value::Null);
        for_agent(&mut v);
        if let Value::Object(o) = &mut v {
            o.insert("sounds".into(), instrument_sounds());
        }
        Ok(ToolOutput::ok(v))
    }
}

/// The beginner instrument list the user sees in Add Instrument: roles, each
/// with its factory sounds. An agent adds one with `instruments_add`
/// (`kind` plugin, this `plugin_id` and `preset`).
fn instrument_sounds() -> Value {
    use plugin_host::sounds;
    let roles: Vec<Value> = sounds::roles()
        .into_iter()
        .map(|role| {
            let items: Vec<Value> = sounds::sounds()
                .iter()
                .filter(|s| s.role == role)
                .filter_map(|s| {
                    let p = sounds::plugin_of(s)?;
                    Some(json!({"name": s.label(), "plugin_id": p.clap_id, "preset": s.preset}))
                })
                .collect();
            json!({"role": role, "sounds": items})
        })
        .collect();
    Value::Array(roles)
}

/// The sound list as an agent reads it: the DAW's fields, plus `kits`
/// (sounds of one kit side by side, so a kick, snare and hat that go
/// together can be picked from one kit), `total`, `notes` and
/// `next_offset` when more match.
fn sounds_json(
    sounds: &[protocol::control::SoundInfo],
    total: u32,
    notes: &[String],
    offset: usize,
) -> Value {
    let items: Vec<Value> = sounds
        .iter()
        .map(|s| {
            let mut item = json!({"id": s.id, "name": s.name, "role": s.role, "tags": s.tags,
                "source": s.source, "kind": s.kind, "family": s.genres.first()});
            if let Some(id) = &s.kit {
                item["kit"] = json!({"id": id, "name": s.kit_name});
            }
            item
        })
        .collect();
    let mut kits: Vec<Value> = Vec::new();
    for it in &items {
        let Some(kit) = it.get("kit") else { continue };
        let one = json!({"id": it["id"], "name": it["name"], "role": it["role"]});
        match kits.iter().position(|k| k["kit"]["id"] == kit["id"]) {
            Some(i) => kits[i]["sounds"].as_array_mut().unwrap().push(one),
            None => kits.push(json!({"kit": kit, "sounds": [one]})),
        }
    }
    kits.retain(|k| k["sounds"].as_array().is_some_and(|s| s.len() > 1));
    let mut v = json!({"sounds": items, "total": total});
    if !kits.is_empty() {
        v["kits"] = json!(kits);
    }
    if !notes.is_empty() {
        v["notes"] = json!(notes);
    }
    if offset + items.len() < total as usize {
        v["next_offset"] = json!(offset + items.len());
    }
    v
}

fn branches_json(current: &str, list: &[BranchInfo]) -> Value {
    let items: Vec<Value> = list
        .iter()
        .map(|b| {
            json!({"branch": b.branch, "name": b.name, "head": short(&b.head), "base": short(&b.base),
                   "author": b.author, "archived": b.archived, "current": b.branch == current})
        })
        .collect();
    json!({"current": current, "branches": items})
}

/// Runs a `tools/call`.
pub fn call_tool(exec: &Exec, name: &str, args: Value) -> Result<ToolOutput, PlanError> {
    let plan = tools::plan(name, args)?;
    Ok(exec.run(plan))
}

fn unexpected(what: &str) -> ToolOutput {
    ToolOutput::error("internal", &format!("unexpected reply to {what}"))
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

fn edit_hint(error: &EditError) -> String {
    match error {
        EditError::Invalid {
            reason: ValidationError::Overlap { a, b },
        } => {
            let who = if *a == 0 {
                "the new or moved clip".to_string()
            } else {
                format!("clip {a}")
            };
            format!(
                "{who} would overlap clip {b} on the same instrument row (clips on one row cannot overlap). Place it after clip {b} ends, shorten one of them, or remove clip {b} first; project_summary shows where clips are."
            )
        }
        EditError::NotFound { what, id } => format!(
            "{} {id} does not exist (maybe the user removed it); call project_summary for the current ids",
            clean_text(what)
        ),
        other => clean_text(&other.to_string()),
    }
}

/// An edit error, with the part of the tool call it came from.
fn edit_error_out(e: &ControlError, labels: &[String]) -> ToolOutput {
    if let ControlError::Edit { index, error } = e {
        let at = index
            .and_then(|i| labels.get(i as usize))
            .map(|l| format!(" at {}", clean_text(l)))
            .unwrap_or_default();
        let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
        for_agent(&mut v);
        return ToolOutput {
            is_error: true,
            value: json!({"error": v, "code": "edit", "message": format!("Nothing was changed. Failed{at}: {}", edit_hint(error))}),
            text: None,
        };
    }
    control_error_out(e)
}

/// The typed error from the DAW plus a sentence telling the agent what to do.
pub fn control_error_out(e: &ControlError) -> ToolOutput {
    let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
    for_agent(&mut v);
    let code = v["code"].as_str().unwrap_or("internal").to_string();
    let hint = match e {
        ControlError::Stale { current } => format!(
            "The project changed since you last read it (the user is editing; it is now revision {current}). Nothing was applied. Read it again (project_summary), check the ids, and retry."
        ),
        ControlError::Busy => {
            "The user is in the middle of an edit gesture (dragging something). Wait a few seconds and retry once.".into()
        }
        ControlError::NeedsUserApproval => "This action needs the user to click Approve in LibreDAW and nobody did within 60 s. Tell the user what you want to do and ask them to approve; do not retry in a loop.".into(),
        ControlError::Denied => "The user denied this action. Do not retry it; ask the user what to do instead.".into(),
        ControlError::NotAllowed => "This request is not allowed for agents.".into(),
        ControlError::Edit { index, error } => match index {
            Some(i) => format!("Edit {i} (counting from 0) failed and nothing in the batch was applied: {}", edit_hint(error)),
            None => format!("The edit was rejected and nothing was changed: {}", edit_hint(error)),
        },
        ControlError::NotFound { what } => format!("{} does not exist. Call project_summary (or branch_list, history) for current ids.", clean_text(what)),
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

/// One short line per bar (bar numbers count from the song start, 0 =
/// start, like every time in these tools): loudness, peak, low/mid/high
/// share and the tracks with a clip playing.
fn bar_table(bars: &[protocol::control::BarLevels]) -> String {
    let mut out = String::from("bar | LUFS | peak dBFS | low/mid/high % | tracks playing\n");
    for b in bars {
        let [lo, mi, hi] = b.band_balance.map(|x| (x * 100.0).round() as i64);
        let tracks: Vec<String> = b.active_tracks.iter().map(u32::to_string).collect();
        out.push_str(&format!(
            "{} | {:.1} | {:.1} | {lo}/{mi}/{hi} | {}\n",
            b.bar,
            b.lufs,
            b.peak_dbfs,
            tracks.join(",")
        ));
    }
    out
}
