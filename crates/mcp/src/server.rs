// SPDX-License-Identifier: GPL-3.0-or-later
//! JSON-RPC 2.0 and MCP lifecycle, and the execution of tool calls.

use protocol::control::{ControlError, Outcome, ReplyBody, RequestBody};
use serde_json::{Value, json};

use crate::PROTOCOL_VERSION;
use crate::conn::{Config, Conn, ConnError};
use crate::sanitize::{clean_text, for_agent};
use crate::tools::{self, Plan, PlanError};

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
/// Used by MCP for requests that arrive before `initialize`.
const NOT_INITIALIZED: i64 = -32002;

pub struct Server {
    conn: Conn,
    initialized: bool,
    /// Latest document revision this server saw in a reply.
    revision: Option<u64>,
}

struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

fn rpc_err(code: i64, message: impl Into<String>) -> RpcError {
    RpcError {
        code,
        message: message.into(),
        data: None,
    }
}

/// The result of a tool call as MCP wants it.
struct ToolOutput {
    is_error: bool,
    value: Value,
}

impl Server {
    pub fn new(config: Config) -> Server {
        Server {
            conn: Conn::new(config),
            initialized: false,
            revision: None,
        }
    }

    /// Handles one line from the client. Returns the line to send back, or
    /// `None` for notifications.
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                return Some(error_line(
                    &Value::Null,
                    rpc_err(PARSE_ERROR, e.to_string()),
                ));
            }
        };
        let Value::Object(obj) = &msg else {
            return Some(error_line(
                &Value::Null,
                rpc_err(INVALID_REQUEST, "expected a JSON-RPC object"),
            ));
        };
        let id = obj.get("id").cloned();
        let method = obj.get("method").and_then(Value::as_str);
        let Some(method) = method else {
            // A response from the client to a request we never sent, or junk.
            return id.map(|id| error_line(&id, rpc_err(INVALID_REQUEST, "missing method")));
        };
        let Some(id) = id else {
            // Notification: nothing to answer (initialized, cancelled, ...).
            return None;
        };
        if !matches!(&id, Value::String(_) | Value::Number(_)) {
            return Some(error_line(
                &Value::Null,
                rpc_err(INVALID_REQUEST, "id must be a string or number"),
            ));
        }
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        let result = self.dispatch(method, params);
        Some(match result {
            Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}).to_string(),
            Err(e) => error_line(&id, e),
        })
    }

    fn dispatch(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            "ping" => Ok(json!({})),
            "initialize" => self.initialize(params),
            _ if !self.initialized => Err(rpc_err(
                NOT_INITIALIZED,
                "server not initialized: send initialize first",
            )),
            "tools/list" => Ok(json!({"tools": tools::definitions()})),
            "tools/call" => self.tools_call(params),
            _ => Err(rpc_err(
                METHOD_NOT_FOUND,
                format!("method not found: {}", clip(method)),
            )),
        }
    }

    fn initialize(&mut self, params: Value) -> Result<Value, RpcError> {
        let requested = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| rpc_err(INVALID_PARAMS, "initialize needs params.protocolVersion"))?;
        if requested != PROTOCOL_VERSION {
            return Err(RpcError {
                code: INVALID_PARAMS,
                message: "Unsupported protocol version".into(),
                data: Some(json!({"supported": [PROTOCOL_VERSION], "requested": clip(requested)})),
            });
        }
        self.initialized = true;
        Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "libredaw-mcp", "version": env!("CARGO_PKG_VERSION")},
            "instructions": tools::INSTRUCTIONS,
        }))
    }

    fn tools_call(&mut self, params: Value) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| rpc_err(INVALID_PARAMS, "tools/call needs params.name"))?
            .to_string();
        let args = params.get("arguments").cloned().unwrap_or(Value::Null);
        if !(args.is_null() || args.is_object()) {
            return Err(rpc_err(INVALID_PARAMS, "arguments must be an object"));
        }
        let plan = match tools::plan(&name, args) {
            Ok(p) => p,
            Err(PlanError::UnknownTool) => {
                return Err(rpc_err(
                    INVALID_PARAMS,
                    format!("unknown tool: {}", clip(&name)),
                ));
            }
            Err(PlanError::BadArguments(m)) => {
                return Ok(tool_result(ToolOutput {
                    is_error: true,
                    value: json!({"error": {"code": "bad_arguments", "message": clean_text(&m)}}),
                }));
            }
        };
        Ok(tool_result(self.run(plan)))
    }

    fn run(&mut self, plan: Plan) -> ToolOutput {
        match plan {
            Plan::Request {
                body,
                base_revision,
            } => self.send(body, base_revision),
            Plan::Steps(p) => {
                let length = match p.length {
                    Some(l) => l,
                    None => match self.pattern_length(p.pattern) {
                        Ok(l) => l,
                        Err(out) => return out,
                    },
                };
                match tools::steps_edits(&p, length) {
                    Ok(edits) => self.send(RequestBody::Edit { edits }, None),
                    Err(m) => error_out("bad_arguments", &m),
                }
            }
        }
    }

    /// Reads the project (which also refreshes the revision) and returns the
    /// length of a pattern.
    fn pattern_length(&mut self, pattern: protocol::ids::PatternId) -> Result<u8, ToolOutput> {
        match self.call(RequestBody::ProjectGet, None) {
            Ok(Outcome::Ok {
                body: ReplyBody::Project { project, .. },
            }) => project
                .pattern(pattern)
                .map(|p| p.length_steps)
                .ok_or_else(|| {
                    error_out(
                        "not_found",
                        &format!("pattern {pattern} does not exist; call project_get for the ids"),
                    )
                }),
            Ok(Outcome::Ok { .. }) => Err(error_out("internal", "unexpected reply to project_get")),
            Ok(Outcome::Err { error }) => Err(control_error_out(&error)),
            Err(e) => Err(conn_error_out(&e)),
        }
    }

    fn send(&mut self, body: RequestBody, base_override: Option<u64>) -> ToolOutput {
        let needs_revision = matches!(
            body,
            RequestBody::Edit { .. } | RequestBody::NotesList { .. }
        );
        let mut base = base_override;
        if needs_revision && base.is_none() {
            base = self.revision;
            if base.is_none() {
                // The agent never read this document: learn its revision.
                if let Err(out) = self.refresh_revision() {
                    return out;
                }
                base = self.revision;
            }
        }
        // Opening or creating a project replaces the document.
        let replaces = matches!(
            body,
            RequestBody::ProjectNew { .. } | RequestBody::ProjectOpen { .. }
        );
        let result = self.call(body, if needs_revision { base } else { None });
        if replaces && matches!(result, Ok(Outcome::Ok { .. })) {
            self.revision = None;
        }
        match result {
            Ok(Outcome::Ok { body }) => {
                let mut v = serde_json::to_value(&body).unwrap_or(Value::Null);
                for_agent(&mut v);
                ToolOutput {
                    is_error: false,
                    value: v,
                }
            }
            Ok(Outcome::Err { error }) => control_error_out(&error),
            Err(e) => conn_error_out(&e),
        }
    }

    fn refresh_revision(&mut self) -> Result<(), ToolOutput> {
        match self.call(RequestBody::ProjectGet, None) {
            Ok(Outcome::Ok { .. }) => Ok(()),
            Ok(Outcome::Err { error }) => Err(control_error_out(&error)),
            Err(e) => Err(conn_error_out(&e)),
        }
    }

    /// One control call; remembers the document revision from the reply.
    fn call(&mut self, body: RequestBody, base: Option<u64>) -> Result<Outcome, ConnError> {
        let out = self.conn.call(body, base)?;
        if let Outcome::Ok { body } = &out {
            match body {
                ReplyBody::Project { revision, .. } | ReplyBody::Job { revision, .. } => {
                    self.revision = Some(*revision)
                }
                ReplyBody::Applied(a) => self.revision = Some(a.revision),
                ReplyBody::Analysis(a) => self.revision = Some(a.revision),
                _ => {}
            }
        }
        Ok(out)
    }
}

fn clip(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(64).collect()
}

fn error_line(id: &Value, e: RpcError) -> String {
    let mut err = json!({"code": e.code, "message": e.message});
    if let Some(d) = e.data {
        err["data"] = d;
    }
    json!({"jsonrpc": "2.0", "id": id, "error": err}).to_string()
}

fn tool_result(out: ToolOutput) -> Value {
    json!({
        "content": [{"type": "text", "text": out.value.to_string()}],
        "structuredContent": out.value,
        "isError": out.is_error,
    })
}

fn error_out(code: &str, message: &str) -> ToolOutput {
    ToolOutput {
        is_error: true,
        value: json!({"error": {"code": code, "message": message}}),
    }
}

fn conn_error_out(e: &ConnError) -> ToolOutput {
    let code = match e {
        ConnError::Waiting => "waiting_for_user",
        ConnError::AgentsDisabled => "agents_disabled",
        ConnError::CannotStart(_) => "daw_not_running",
        ConnError::Refused(_) => "refused",
        ConnError::Lost(_) => "connection_lost",
    };
    error_out(code, &clean_text(&e.to_string()))
}

/// The typed error from the DAW plus a sentence telling the agent what to do.
fn control_error_out(e: &ControlError) -> ToolOutput {
    let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
    for_agent(&mut v);
    let code = v["code"].as_str().unwrap_or("internal").to_string();
    let hint = match e {
        ControlError::Stale { current } => format!(
            "The document changed since you last read it (now revision {current}). Call project_get, check the ids, and retry."
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
        ControlError::NotFound { .. } => "That item does not exist. Call project_get for current ids.".into(),
        ControlError::TooLarge { .. } => "The request is too large; split it into smaller batches.".into(),
        ControlError::BadRequest { reason } => format!("Bad request: {}", clean_text(reason)),
        ControlError::Internal { reason } => format!("LibreDAW internal error: {}", clean_text(reason)),
    };
    ToolOutput {
        is_error: true,
        value: json!({"error": v, "code": code, "message": hint}),
    }
}
