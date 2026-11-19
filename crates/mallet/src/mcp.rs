//! Model Context Protocol server that lets AI agents drive the running editor.
//!
//! Speaks the Streamable HTTP transport with plain JSON responses on
//! `http://127.0.0.1:<port>/mcp`. Only loopback connections are accepted, and requests
//! carrying a non-local `Origin` header are refused to block DNS-rebinding attacks.
//!
//! Each `tools/call` runs on this server's thread through the handler given to [`start`],
//! which works on the editor state the window shares, so agent edits go through the same
//! undo history as edits made by hand. A panicking tool becomes an error result instead of
//! stopping the server.

use std::io::{self, Read};
use std::panic::{self, AssertUnwindSafe};
use std::thread;

use base64::Engine;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

/// Protocol revisions this server understands, newest first.
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
/// Largest request body accepted.
const MAX_BODY_BYTES: u64 = 16 * 1024 * 1024;
/// The longest line [`json_text`] puts an array or object on before it breaks it up.
const JSON_LINE_WIDTH: usize = 180;
/// How many decimal places [`json_text`] keeps of a number.
const JSON_DECIMALS: i32 = 6;

const INSTRUCTIONS: &str = "Mallet is a Hammer-style editor for Source engine VMF maps. \
Use get_status first to see which map is open. Edits made through these tools are undoable \
in the editor. Use screenshot to see the camera, top, front and side viewports. \
Unless the user turns it off in Mallet Options > AI agents, you work in your own view: \
your tab, camera, 2D views, grid, selection and tools are yours, the map is shared, and \
tools that would drive the window (send_input and the show_ tools) are refused.";

/// One piece of a tool result.
pub enum Content {
    Text(String),
    Png(Vec<u8>),
}

/// The answer to a tool call.
pub struct ToolResult {
    pub content: Vec<Content>,
    pub is_error: bool,
}

impl ToolResult {
    /// A successful text result.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![Content::Text(text.into())],
            is_error: false,
        }
    }

    /// A successful result holding JSON, laid out by [`json_text`].
    pub fn json(value: &Value) -> Self {
        Self::text(json_text(value))
    }

    /// A failed tool call with a message the agent can act on.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: vec![Content::Text(message.into())],
            is_error: true,
        }
    }

    /// A PNG image.
    pub fn png(bytes: Vec<u8>) -> Self {
        Self {
            content: vec![Content::Png(bytes)],
            is_error: false,
        }
    }

    fn to_json(&self) -> Value {
        let content: Vec<Value> = self
            .content
            .iter()
            .map(|item| match item {
                Content::Text(text) => json!({ "type": "text", "text": text }),
                Content::Png(bytes) => json!({
                    "type": "image",
                    "mimeType": "image/png",
                    "data": base64::engine::general_purpose::STANDARD.encode(bytes),
                }),
            })
            .collect();
        json!({ "content": content, "isError": self.is_error })
    }
}

/// JSON laid out for an agent to read at little cost: an array or object that fits on
/// one line stays on it, so a point is `[0, 64, 128]` and a short record one line of a
/// list, and only what is longer is broken up, one item to a line, or as many as fit
/// when the items are plain numbers or strings, such as a list of ids. Pretty-printing
/// every number onto its own line takes several times the text for the same answer. Numbers
/// are written as short as they can be: whole ones without a decimal point, and the
/// others rounded to [`JSON_DECIMALS`] places, which drops the noise a 32-bit
/// coordinate picks up on its way to 64 bits.
pub fn json_text(value: &Value) -> String {
    let mut text = String::new();
    write_json(value, 0, 0, &mut text);
    text
}

/// Writes `value` at nesting depth `depth`, where `column` characters of its first line
/// are taken already.
fn write_json(value: &Value, depth: usize, column: usize, text: &mut String) {
    let items: Vec<(Option<&String>, &Value)> = match value {
        Value::Array(items) => items.iter().map(|item| (None, item)).collect(),
        Value::Object(entries) => entries.iter().map(|(key, item)| (Some(key), item)).collect(),
        scalar => return text.push_str(&scalar_text(scalar)),
    };
    let (open, close) = if value.is_array() { ('[', ']') } else { ('{', '}') };
    let line = one_line(value);
    if items.is_empty() || column + line.len() <= JSON_LINE_WIDTH {
        return text.push_str(&line);
    }
    text.push(open);
    if value.is_array() && items.iter().all(|(_, item)| !item.is_array() && !item.is_object()) {
        let mut taken = JSON_LINE_WIDTH;
        for (index, (_, item)) in items.iter().enumerate() {
            let mut item = scalar_text(item);
            if index + 1 < items.len() {
                item.push(',');
            }
            if taken + 1 + item.len() > JSON_LINE_WIDTH {
                text.push('\n');
                text.push_str(&" ".repeat(depth + 1));
                taken = depth + 1;
            } else {
                text.push(' ');
                taken += 1;
            }
            taken += item.len();
            text.push_str(&item);
        }
        text.push('\n');
        text.push_str(&" ".repeat(depth));
        return text.push(close);
    }
    for (index, (key, item)) in items.iter().enumerate() {
        text.push('\n');
        text.push_str(&" ".repeat(depth + 1));
        let mut taken = depth + 1;
        if let Some(key) = key {
            let key = Value::String((*key).clone()).to_string();
            taken += key.len() + 2;
            text.push_str(&key);
            text.push_str(": ");
        }
        write_json(item, depth + 1, taken, text);
        if index + 1 < items.len() {
            text.push(',');
        }
    }
    text.push('\n');
    text.push_str(&" ".repeat(depth));
    text.push(close);
}

/// `value` on one line, with a space after every comma and colon.
fn one_line(value: &Value) -> String {
    match value {
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(one_line).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(entries) => {
            let entries: Vec<String> = entries
                .iter()
                .map(|(key, item)| format!("{}: {}", Value::String(key.clone()), one_line(item)))
                .collect();
            format!("{{{}}}", entries.join(", "))
        }
        scalar => scalar_text(scalar),
    }
}

/// A value that is neither an array nor an object, as [`json_text`] writes it.
fn scalar_text(value: &Value) -> String {
    let Some(number) = value.as_f64().filter(|number| value.is_f64() && number.is_finite()) else {
        return value.to_string();
    };
    let places = 10_f64.powi(JSON_DECIMALS);
    let rounded = (number * places).round() / places;
    if rounded.abs() >= 1e15 {
        value.to_string()
    } else if rounded.fract() == 0.0 {
        format!("{}", rounded as i64)
    } else {
        format!("{rounded}")
    }
}

/// Runs a tool by name with its JSON arguments.
pub trait ToolHandler: Fn(&str, &Value) -> ToolResult + Send + 'static {}

impl<F: Fn(&str, &Value) -> ToolResult + Send + 'static> ToolHandler for F {}

/// Starts the server on a background thread and returns its endpoint URL.
///
/// `tools` is the JSON array returned from `tools/list`; `handler` runs each tool call.
/// The port the server listens on unless Mallet Options or `MALLET_MCP_PORT` says
/// otherwise.
pub const DEFAULT_PORT: u16 = 7414;

/// The port from `MALLET_MCP_PORT`, or else the one in Mallet Options; `None` when it is
/// 0 or `off`, or when the options turn the server off, which leaves it off.
pub fn configured_port(options: &crate::config::AgentOptions) -> Result<Option<u16>, String> {
    let setting = std::env::var("MALLET_MCP_PORT").ok();
    match setting.as_deref().map(str::trim) {
        None | Some("") if !options.enabled || options.port == 0 => Ok(None),
        None | Some("") => Ok(Some(options.port)),
        Some("off") => Ok(None),
        Some(value) => match value.parse() {
            Ok(0) => Ok(None),
            Ok(port) => Ok(Some(port)),
            Err(_) => Err(format!("MALLET_MCP_PORT \"{value}\" is not a port number")),
        },
    }
}

pub fn start(port: u16, tools: Value, handler: impl ToolHandler) -> io::Result<String> {
    let server = Server::http(("127.0.0.1", port)).map_err(io::Error::other)?;
    thread::Builder::new().name("mcp".to_owned()).spawn(move || {
        for request in server.incoming_requests() {
            handle(request, &handler, &tools);
        }
    })?;
    Ok(format!("http://127.0.0.1:{port}/mcp"))
}

fn handle(mut request: Request, handler: &dyn ToolHandler, tools: &Value) {
    let path = request.url().split('?').next().unwrap_or("");
    let response = if path != "/mcp" {
        Response::from_string("not found").with_status_code(404)
    } else if !origin_allowed(&request) {
        Response::from_string("origin not allowed").with_status_code(403)
    } else if *request.method() != Method::Post {
        Response::from_string("this server only accepts POST")
            .with_status_code(405)
            .with_header(header("Allow", "POST"))
    } else {
        let mut body = String::new();
        let read = request.as_reader().take(MAX_BODY_BYTES).read_to_string(&mut body);
        match read {
            Ok(_) => answer(&body, handler, tools),
            Err(error) => Response::from_string(error.to_string()).with_status_code(400),
        }
    };
    let _ = request.respond(response);
}

fn answer(body: &str, handler: &dyn ToolHandler, tools: &Value) -> Response<io::Cursor<Vec<u8>>> {
    let message: Value = match serde_json::from_str(body) {
        Ok(message) => message,
        Err(error) => return rpc_response(Value::Null, Err((-32700, format!("parse error: {error}")))),
    };
    let Some(id) = message.get("id").cloned() else {
        return Response::from_data(Vec::new()).with_status_code(202);
    };
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    rpc_response(id, dispatch(method, &params, handler, tools))
}

fn dispatch(method: &str, params: &Value, handler: &dyn ToolHandler, tools: &Value) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => {
            let requested = params.get("protocolVersion").and_then(Value::as_str);
            let version = requested
                .filter(|version| PROTOCOL_VERSIONS.contains(version))
                .unwrap_or(PROTOCOL_VERSIONS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": "mallet",
                    "title": "Mallet",
                    "version": env!("CARGO_PKG_VERSION"),
                    "icons": [{ "src": crate::branding::icon_data_url(), "mimeType": "image/png", "sizes": ["64x64"] }],
                },
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or((-32602, "missing tool name".to_owned()))?;
            let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let result = panic::catch_unwind(AssertUnwindSafe(|| handler(name, &arguments)))
                .unwrap_or_else(|_| ToolResult::error(format!("the \"{name}\" tool crashed")));
            Ok(result.to_json())
        }
        _ => Err((-32601, format!("method not found: {method}"))),
    }
}

fn rpc_response(id: Value, result: Result<Value, (i64, String)>) -> Response<io::Cursor<Vec<u8>>> {
    let body = match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    };
    Response::from_data(body.to_string().into_bytes()).with_header(header("Content-Type", "application/json"))
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("static header is valid")
}

/// Allows requests without an `Origin` header and requests from loopback origins only.
fn origin_allowed(request: &Request) -> bool {
    let Some(origin) = request.headers().iter().find(|header| header.field.equiv("Origin")) else {
        return true;
    };
    let origin = origin.value.as_str();
    let host = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    let host = if host.starts_with('[') {
        host.split_inclusive(']').next().unwrap_or("")
    } else {
        host.split([':', '/']).next().unwrap_or("")
    };
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_arrays_and_records_stay_on_one_line() {
        assert_eq!(json_text(&json!([0, 64.5, -128])), "[0, 64.5, -128]");
        assert_eq!(
            json_text(&json!({ "id": 7, "origin": [0, 0, 16] })),
            "{\"id\": 7, \"origin\": [0, 0, 16]}"
        );
        assert_eq!(json_text(&json!({})), "{}");
        assert_eq!(json_text(&json!("a \"quoted\" word")), "\"a \\\"quoted\\\" word\"");
    }

    #[test]
    fn numbers_are_written_as_short_as_they_can_be() {
        let numbers = json!([
            256.0,
            -0.0,
            0.25,
            141.26136207580566,
            0.7071067811865476,
            1e-9,
            12345678901_u64,
            -3
        ]);
        assert_eq!(
            json_text(&numbers),
            "[256, 0, 0.25, 141.261362, 0.707107, 0, 12345678901, -3]"
        );
        assert_eq!(
            json_text(&json!({ "big": 1e20, "none": null, "on": true })),
            "{\"big\": 1e+20, \"none\": null, \"on\": true}"
        );
    }

    #[test]
    fn long_answers_break_into_one_item_a_line() {
        let entities: Vec<Value> = (0..4)
            .map(|id| json!({ "id": id, "classname": "prop_static", "origin": [id * 64, 0, 16], "targetname": null }))
            .collect();
        let answer = json!({ "total": 4, "entities": entities });
        let text = json_text(&answer);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 9, "each entity takes one line:\n{text}");
        assert_eq!(lines[0], "{");
        assert_eq!(lines[1], " \"entities\": [");
        assert!(lines[2].starts_with("  {\"classname\": \"prop_static\", \"id\": 0, \"origin\": [0, 0, 16]"));
        assert_eq!(lines[7], " \"total\": 4");
        assert!(lines.iter().all(|line| line.len() <= JSON_LINE_WIDTH));
        assert!(json_text(&answer["entities"][0]).lines().count() == 1);
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            answer,
            "it is still the same JSON"
        );
        let pretty_lines = serde_json::to_string_pretty(&answer).unwrap().lines().count();
        assert!(
            pretty_lines > 4 * lines.len(),
            "pretty-printing takes {pretty_lines} lines"
        );
    }

    #[test]
    fn long_lists_of_numbers_and_names_fill_their_lines() {
        let ids: Vec<u32> = (1000..1200).collect();
        let text = json_text(&json!({ "solids": ids }));
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.len() < 12, "200 ids take a few lines, not 200:\n{text}");
        assert_eq!(lines[1], " \"solids\": [");
        assert!(lines[2].starts_with("  1000, 1001, 1002,"));
        assert!(lines.iter().all(|line| line.len() <= JSON_LINE_WIDTH));
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap()["solids"]
                .as_array()
                .unwrap()
                .len(),
            200
        );
        let names: Vec<String> = (0..40).map(|number| format!("brick/brickwall{number:03}a")).collect();
        let text = json_text(&json!(names));
        assert!(text.lines().count() < 10);
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), json!(names));
    }

    #[test]
    fn a_long_string_is_not_broken() {
        let word = "x".repeat(300);
        let text = json_text(&json!({ "vmt": word, "size": [64, 64] }));
        assert_eq!(text.lines().count(), 4);
        assert!(text.contains("\"size\": [64, 64],"));
    }
}
