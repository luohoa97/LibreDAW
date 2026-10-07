// SPDX-License-Identifier: GPL-3.0-or-later
//! The MCP side of one client connection: JSON-RPC dispatch.
//!
//! Requests that only look at server state are answered on the reader
//! thread. Requests that wait for the UI (`tools/call`, `resources/read`,
//! `resources/list`) run on a worker thread each, so `ping`, notifications
//! and further calls are never stuck behind a human approval.

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use protocol::consts::MAX_REQUEST_LINE_BYTES;
use protocol::control::{Transport, agent_string};
use serde_json::{Value, json};

use super::exec::{Exec, ToolOutput, call_tool};
use super::sanitize::clean_text;
use super::session::{SUGGESTIONS_URI, Session};
use super::{PROTOCOL_VERSION, prompts, resources, tools};
use crate::client::{LineReader, ReadLine};
use crate::state::{HelloErr, MAX_IN_FLIGHT, Out, Shared};
use crate::suggest;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
/// MCP: request before `initialize`.
const NOT_INITIALIZED: i64 = -32002;
/// MCP: resource not found.
const RESOURCE_NOT_FOUND: i64 = -32002;
/// Server errors for the agent-control gate.
const AGENTS_DISABLED: i64 = -32001;
const REFUSED: i64 = -32000;

pub(crate) struct RpcError {
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

fn clip(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(64).collect()
}

fn response(id: &Value, r: Result<Value, RpcError>) -> Value {
    match r {
        Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
        Err(e) => {
            let mut err = json!({"code": e.code, "message": e.message});
            if let Some(d) = e.data {
                err["data"] = d;
            }
            json!({"jsonrpc": "2.0", "id": id, "error": err})
        }
    }
}

struct Ctx {
    shared: Arc<Shared>,
    session: Arc<Session>,
}

/// Serves one MCP connection until it ends. `first` is the first line,
/// already read.
pub(crate) fn serve(
    shared: Arc<Shared>,
    reader: &mut LineReader,
    tx: Sender<Out>,
    kill: UnixStream,
    id: u64,
    first: String,
) {
    let session = Arc::new(Session::new(id, tx.clone()));
    let ctx = Arc::new(Ctx {
        shared: Arc::clone(&shared),
        session: Arc::clone(&session),
    });
    let mut pending = Some(first);
    loop {
        let line = match pending.take() {
            Some(l) => l,
            None => match reader.read_line(MAX_REQUEST_LINE_BYTES) {
                ReadLine::Line(l) => l,
                ReadLine::NotUtf8 => {
                    session.send(&response(
                        &Value::Null,
                        Err(rpc_err(PARSE_ERROR, "message is not UTF-8")),
                    ));
                    continue;
                }
                ReadLine::TooLong => {
                    session.send(&response(
                        &Value::Null,
                        Err(rpc_err(
                            INVALID_REQUEST,
                            format!("message is longer than {MAX_REQUEST_LINE_BYTES} bytes"),
                        )),
                    ));
                    let _ = tx.send(Out::Close);
                    reader.drain();
                    break;
                }
                ReadLine::Timeout => continue,
                ReadLine::Closed => break,
            },
        };
        if line.trim().is_empty() {
            continue;
        }
        handle_line(&ctx, &kill, &line);
    }
    shared.unregister(id);
    session.close();
    let _ = tx.send(Out::Close);
}

fn handle_line(ctx: &Arc<Ctx>, kill: &UnixStream, line: &str) {
    let session = &ctx.session;
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            session.send(&response(
                &Value::Null,
                Err(rpc_err(PARSE_ERROR, clean_text(&e.to_string()))),
            ));
            return;
        }
    };
    let Value::Object(obj) = &msg else {
        session.send(&response(
            &Value::Null,
            Err(rpc_err(INVALID_REQUEST, "expected a JSON-RPC object")),
        ));
        return;
    };
    let id = obj.get("id").cloned();
    if let Some(method) = obj.get("method").and_then(Value::as_str) {
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        // Notifications (no id: initialized, cancelled, ...) need no answer.
        if let Some(id) = id {
            if !matches!(&id, Value::String(_) | Value::Number(_)) {
                session.send(&response(
                    &Value::Null,
                    Err(rpc_err(INVALID_REQUEST, "id must be a string or number")),
                ));
                return;
            }
            request(ctx, kill, method, params, id);
        }
        return;
    }
    // A response to a request we sent (sampling).
    if let Some(Value::String(rid)) = &id {
        if let Some(result) = obj.get("result") {
            session.client_response(rid, Ok(result.clone()));
        } else if let Some(err) = obj.get("error") {
            session.client_response(rid, Err(err.clone()));
        }
        return;
    }
    if let Some(id) = id {
        session.send(&response(
            &id,
            Err(rpc_err(INVALID_REQUEST, "missing method")),
        ));
    }
}

fn request(ctx: &Arc<Ctx>, kill: &UnixStream, method: &str, params: Value, id: Value) {
    let session = &ctx.session;
    let reply = |r: Result<Value, RpcError>| session.send(&response(&id, r));
    match method {
        "ping" => return reply(Ok(json!({}))),
        "initialize" => return reply(initialize(ctx, kill, &params)),
        _ if !session.initialized() => {
            return reply(Err(rpc_err(
                NOT_INITIALIZED,
                "server not initialized: send initialize first",
            )));
        }
        "tools/list" => return reply(Ok(json!({"tools": tools::definitions()}))),
        "prompts/list" => return reply(Ok(prompts::list())),
        "prompts/get" => return reply(prompts_get(&params)),
        "resources/templates/list" => return reply(Ok(resources::templates())),
        "resources/subscribe" | "resources/unsubscribe" => {
            return reply(subscribe(session, method == "resources/subscribe", &params));
        }
        "logging/setLevel" => {
            let level = params.get("level").and_then(Value::as_str).unwrap_or("");
            return reply(if session.set_log_level(level) {
                Ok(json!({}))
            } else {
                Err(rpc_err(
                    INVALID_PARAMS,
                    "level must be one of debug, info, notice, warning, error, critical, alert, emergency",
                ))
            });
        }
        "tools/call" | "resources/read" | "resources/list" => {}
        _ => {
            return reply(Err(rpc_err(
                METHOD_NOT_FOUND,
                format!("method not found: {}", clip(method)),
            )));
        }
    }
    // These wait for the UI: one worker thread each.
    if session.in_flight.fetch_add(1, Ordering::SeqCst) >= MAX_IN_FLIGHT {
        session.in_flight.fetch_sub(1, Ordering::SeqCst);
        return reply(Err(rpc_err(
            REFUSED,
            "too many requests in flight: wait for earlier calls to finish",
        )));
    }
    let ctx = Arc::clone(ctx);
    let method = method.to_string();
    let spawned = thread::Builder::new()
        .name("control-mcp-call".into())
        .spawn({
            let ctx = Arc::clone(&ctx);
            let id = id.clone();
            move || {
                let r = match method.as_str() {
                    "tools/call" => tools_call(&ctx, &params),
                    "resources/read" => resources_read(&ctx, &params),
                    _ => resources_list(&ctx),
                };
                ctx.session.send(&response(&id, r));
                ctx.session.in_flight.fetch_sub(1, Ordering::SeqCst);
            }
        });
    if spawned.is_err() {
        ctx.session.in_flight.fetch_sub(1, Ordering::SeqCst);
        ctx.session.send(&response(
            &id,
            Err(rpc_err(REFUSED, "cannot start a worker for this call")),
        ));
    }
}

fn initialize(ctx: &Ctx, kill: &UnixStream, params: &Value) -> Result<Value, RpcError> {
    if ctx.session.initialized() {
        return Err(rpc_err(INVALID_REQUEST, "already initialized"));
    }
    if params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .is_none()
    {
        return Err(rpc_err(
            INVALID_PARAMS,
            "initialize needs params.protocolVersion",
        ));
    }
    let name = agent_string(
        params
            .pointer("/clientInfo/name")
            .and_then(Value::as_str)
            .unwrap_or("mcp-client"),
    );
    let sampling = params
        .pointer("/capabilities/sampling")
        .is_some_and(Value::is_object);
    // The user may be about to enable agent control (banner after
    // `--agent-request`): wait for that before giving up with a clear error.
    let deadline = Instant::now() + ctx.shared.agents_wait;
    loop {
        let stream = kill
            .try_clone()
            .map_err(|e| rpc_err(REFUSED, format!("cannot use the connection: {e}")))?;
        match ctx.shared.register(
            ctx.session.client_id(),
            Transport::Agent,
            name.clone(),
            tx_of(&ctx.session),
            stream,
            Some(Arc::clone(&ctx.session)),
        ) {
            Ok(_) => break,
            Err(HelloErr::AgentsDisabled) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(HelloErr::AgentsDisabled) => {
                return Err(RpcError {
                    code: AGENTS_DISABLED,
                    message: "agent control is not enabled in LibreDAW: ask the user to enable \"Allow agents to control LibreDAW\" (the banner in the LibreDAW window or the preferences), then connect again".into(),
                    data: Some(json!({"reason": "agents_disabled"})),
                });
            }
            Err(HelloErr::BusyOwner) => {
                return Err(RpcError {
                    code: REFUSED,
                    message: "another agent session is already connected to LibreDAW; only one agent can control it at a time".into(),
                    data: Some(json!({"reason": "busy_owner"})),
                });
            }
            Err(HelloErr::NotAllowed) => {
                return Err(rpc_err(REFUSED, "this connection is not allowed"));
            }
        }
    }
    ctx.session.set_initialized(sampling);
    Ok(json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {
            "tools": {"listChanged": false},
            "resources": {"subscribe": true, "listChanged": false},
            "prompts": {"listChanged": false},
            "logging": {},
        },
        "serverInfo": {"name": "libredaw", "title": "LibreDAW", "version": env!("CARGO_PKG_VERSION")},
        "instructions": tools::INSTRUCTIONS,
    }))
}

fn tx_of(session: &Session) -> Sender<Out> {
    session.sender()
}

fn prompts_get(params: &Value) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| rpc_err(INVALID_PARAMS, "prompts/get needs params.name"))?;
    let empty = serde_json::Map::new();
    let args = params
        .get("arguments")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    prompts::get(name, args).map_err(|m| rpc_err(INVALID_PARAMS, m))
}

fn subscribe(session: &Session, on: bool, params: &Value) -> Result<Value, RpcError> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| rpc_err(INVALID_PARAMS, "needs params.uri"))?;
    if !resources::is_known_uri(uri) {
        return Err(rpc_err(
            RESOURCE_NOT_FOUND,
            format!("unknown resource: {}", clip(uri)),
        ));
    }
    if on {
        session.subscribe(uri);
    } else {
        session.unsubscribe(uri);
    }
    Ok(json!({}))
}

fn tools_call(ctx: &Ctx, params: &Value) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| rpc_err(INVALID_PARAMS, "tools/call needs params.name"))?
        .to_string();
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);
    if !(args.is_null() || args.is_object()) {
        return Err(rpc_err(INVALID_PARAMS, "arguments must be an object"));
    }
    let mut exec = Exec::new(Arc::clone(&ctx.shared), Arc::clone(&ctx.session));
    exec.progress_token = params.pointer("/_meta/progressToken").cloned();
    match call_tool(&exec, &name, args) {
        Ok(out) => Ok(out.into_result()),
        Err(tools::PlanError::UnknownTool) => Err(rpc_err(
            INVALID_PARAMS,
            format!("unknown tool: {}", clip(&name)),
        )),
        Err(tools::PlanError::BadArguments(m)) => {
            Ok(ToolOutput::error("bad_arguments", &clean_text(&m)).into_result())
        }
    }
}

fn resources_list(ctx: &Ctx) -> Result<Value, RpcError> {
    let mut list = resources::fixed();
    let exec = Exec::new(Arc::clone(&ctx.shared), Arc::clone(&ctx.session));
    if let Ok((_, project)) = exec.project() {
        list.extend(resources::pattern_entries(&project));
    }
    Ok(json!({"resources": list}))
}

fn resources_read(ctx: &Ctx, params: &Value) -> Result<Value, RpcError> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| rpc_err(INVALID_PARAMS, "resources/read needs params.uri"))?;
    if !resources::is_known_uri(uri) {
        return Err(rpc_err(
            RESOURCE_NOT_FOUND,
            format!("unknown resource: {}", clip(uri)),
        ));
    }
    let (mime, text) = if uri == SUGGESTIONS_URI {
        ("application/json", suggest::pending_resource(&ctx.shared))
    } else {
        let exec = Exec::new(Arc::clone(&ctx.shared), Arc::clone(&ctx.session));
        let (rev, project) = exec.project().map_err(|o| rpc_err(REFUSED, o.message()))?;
        match resources::read_project_resource(uri, &project, rev) {
            Some(t) => ("text/plain", t),
            None => {
                return Err(rpc_err(
                    RESOURCE_NOT_FOUND,
                    format!("that pattern does not exist: {}", clip(uri)),
                ));
            }
        }
    };
    Ok(json!({"contents": [{"uri": uri, "mimeType": mime, "text": text}]}))
}
