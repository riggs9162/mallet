//! The `batch` tool: several tool calls in one request, so an agent builds or checks
//! something in one round trip instead of one per call, and its edits undo as one step.
//!
//! Each call runs exactly as it would alone, taking the editor only for its own length,
//! so the window keeps drawing between the calls of a long batch.

use std::panic::{self, AssertUnwindSafe};
use std::sync::Mutex;

use serde_json::{json, Value};

use super::{optional_string, tool, Called};
use crate::editor::{self, Editor, HistoryMark};
use crate::mcp::{Content, ToolResult};

/// The most calls one batch runs.
const MAX_CALLS: usize = 200;

/// The `tools/list` entry of the batch tool.
pub fn definition() -> Value {
    tool(
        "batch",
        "Run several tool calls in one request, in order, each as it would run alone, and get all their \
         answers at once: use it to build something from many steps or to ask several things without a \
         round trip for each. A call acts on what the calls before it left, such as the selection a \
         create_ tool makes. The batch's edits to the map become one undo step. on_error says what a \
         failing call does: stop (default) leaves the rest undone, continue runs them all the same, and \
         undo also takes back what the batch has changed so far. results picks which answers come back: \
         all (default), errors for the failed calls only, or last for those and the last call's. Pictures \
         the calls answer with always come back, in order after the answers. A batch cannot hold another \
         batch.",
        json!({
            "calls": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_CALLS,
                "items": {
                    "type": "object",
                    "properties": {
                        "tool": { "type": "string", "description": "The tool's name." },
                        "arguments": { "type": "object", "description": "The tool's arguments; default none." }
                    },
                    "required": ["tool"]
                }
            },
            "on_error": { "type": "string", "enum": ["stop", "continue", "undo"] },
            "results": { "type": "string", "enum": ["all", "errors", "last"] }
        }),
        &["calls"],
    )
}

/// What a failing call does to the rest of its batch.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OnError {
    Stop,
    Continue,
    Undo,
}

/// Which answers a batch sends back.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Report {
    All,
    Errors,
    Last,
}

/// Follows what a batch's calls add to the undo history of the map they work on, to
/// join it into one step afterwards.
#[derive(Default)]
struct Steps {
    /// Where the history stood after the batch's latest change.
    latest: Option<HistoryMark>,
    /// How many undo steps the batch has added.
    added: usize,
    /// Why the steps cannot be joined or taken back together, once that is so.
    apart: Option<&'static str>,
}

impl Steps {
    /// Takes in what one call did to the history, given where it stood before and after.
    fn note(&mut self, history: Option<(HistoryMark, HistoryMark)>) {
        let Some((before, after)) = history else {
            return;
        };
        if before == after {
            return;
        }
        if self.latest.is_some_and(|latest| latest != before) {
            self.apart = Some("the map changed between the batch's calls, or they worked on different maps");
        }
        if after.map != before.map {
            self.apart = Some("a call moved on to another map");
        } else if after.steps < before.steps {
            self.apart = Some("a call undid steps");
        } else {
            self.added += after.steps - before.steps;
        }
        self.latest = Some(after);
    }

    /// Joins the batch's steps into one. Returns how many undo steps the batch leaves
    /// and, when they stay apart, why.
    fn join(&mut self, shared: &Mutex<Editor>) -> (usize, Option<String>) {
        if self.added < 2 {
            return (self.added, None);
        }
        let joined = match (self.apart, self.latest) {
            (None, Some(latest)) => editor::lock(shared).join_tool_history(latest, self.added),
            _ => false,
        };
        if joined {
            return (1, None);
        }
        let reason = self.apart.unwrap_or("the map changed after the batch's last call");
        (
            self.added,
            Some(format!("The batch's edits stay {} undo steps: {reason}.", self.added)),
        )
    }

    /// Takes back everything the batch changed. Returns how many undo steps the batch
    /// leaves, and what happened.
    fn undo(&mut self, shared: &Mutex<Editor>) -> (usize, Option<String>) {
        if self.added == 0 {
            return (0, None);
        }
        let mut editor = editor::lock(shared);
        let untouched = self.latest.is_some() && editor.tool_history_mark() == self.latest;
        if let (None, true) = (self.apart, untouched) {
            let steps = self.added;
            let undone = super::in_view(&mut editor, |editor| {
                (0..steps).try_for_each(|_| editor.step_history(false).map(|_| ()))
            });
            if undone.is_ok() {
                return (
                    0,
                    Some(format!("A call failed, so the batch's {steps} edits were undone.")),
                );
            }
        }
        let reason = self.apart.unwrap_or("the map changed after the batch's last call");
        (
            self.added,
            Some(format!(
                "A call failed, and the batch's {} edits could not be undone together: {reason}. Undo them with the undo tool.",
                self.added
            )),
        )
    }
}

/// One call's answer as an entry of the batch's answer. Pictures go to `pictures`, and
/// the entry says which they are.
fn entry(name: &str, result: ToolResult, pictures: &mut Vec<Vec<u8>>) -> Value {
    let mut texts: Vec<String> = Vec::new();
    let mut shown: Vec<usize> = Vec::new();
    for content in result.content {
        match content {
            Content::Text(text) => texts.push(text),
            Content::Png(png) => {
                pictures.push(png);
                shown.push(pictures.len());
            }
        }
    }
    let text = texts.join("\n");
    let mut entry = json!({ "tool": name });
    if result.is_error {
        entry["error"] = json!(text);
    } else if !text.is_empty() {
        entry["result"] = serde_json::from_str(&text).unwrap_or(Value::String(text));
    }
    match shown.as_slice() {
        [] => {}
        [one] => entry["picture"] = json!(one),
        many => entry["pictures"] = json!(many),
    }
    entry
}

/// Runs a batch. `call` runs one tool call the way the server runs it alone.
pub fn run(shared: &Mutex<Editor>, args: &Value, call: &mut dyn FnMut(&str, &Value) -> Called) -> ToolResult {
    match run_calls(shared, args, call) {
        Ok(result) => result,
        Err(message) => ToolResult::error(message),
    }
}

fn run_calls(
    shared: &Mutex<Editor>,
    args: &Value,
    call: &mut dyn FnMut(&str, &Value) -> Called,
) -> Result<ToolResult, String> {
    let calls = args
        .get("calls")
        .and_then(Value::as_array)
        .filter(|calls| !calls.is_empty())
        .ok_or("\"calls\" must be a list of calls, each with a \"tool\" and its \"arguments\"")?;
    if calls.len() > MAX_CALLS {
        return Err(format!(
            "a batch runs at most {MAX_CALLS} calls; this one has {}",
            calls.len()
        ));
    }
    let on_error = match optional_string(args, "on_error")?.unwrap_or("stop") {
        "stop" => OnError::Stop,
        "continue" => OnError::Continue,
        "undo" => OnError::Undo,
        other => return Err(format!("\"on_error\" must be stop, continue or undo, not \"{other}\"")),
    };
    let report = match optional_string(args, "results")?.unwrap_or("all") {
        "all" => Report::All,
        "errors" => Report::Errors,
        "last" => Report::Last,
        other => return Err(format!("\"results\" must be all, errors or last, not \"{other}\"")),
    };
    let mut planned = Vec::with_capacity(calls.len());
    for (index, spec) in calls.iter().enumerate() {
        let name = spec
            .get("tool")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("call {index} names no \"tool\""))?;
        if name == "batch" {
            return Err(format!(
                "call {index} is a batch, and a batch cannot hold another batch"
            ));
        }
        let arguments = match spec.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(arguments) if arguments.is_object() => arguments.clone(),
            Some(_) => return Err(format!("the \"arguments\" of call {index} must be an object")),
        };
        planned.push((name, arguments));
    }

    let mut steps = Steps::default();
    let mut pictures: Vec<Vec<u8>> = Vec::new();
    let mut entries: Vec<Value> = Vec::new();
    let mut ran = 0;
    let mut failed = 0;
    for (index, (name, arguments)) in planned.iter().enumerate() {
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| call(name, arguments))).unwrap_or_else(|_| Called {
            result: ToolResult::error(format!("the \"{name}\" tool crashed")),
            history: None,
        });
        ran += 1;
        steps.note(outcome.history);
        let is_error = outcome.result.is_error;
        let last = index + 1 == planned.len();
        let wanted = match report {
            Report::All => true,
            Report::Errors => is_error,
            Report::Last => is_error || last,
        };
        let pictured = outcome
            .result
            .content
            .iter()
            .any(|content| matches!(content, Content::Png(_)));
        if wanted || pictured {
            let mut entry = entry(name, outcome.result, &mut pictures);
            if let (false, Some(fields)) = (wanted, entry.as_object_mut()) {
                fields.remove("result");
            }
            entry["call"] = json!(index);
            entries.push(entry);
        }
        if is_error {
            failed += 1;
            if on_error != OnError::Continue {
                break;
            }
        }
    }
    let (undo_steps, note) = if failed > 0 && on_error == OnError::Undo {
        steps.undo(shared)
    } else {
        steps.join(shared)
    };
    let mut summary = json!({
        "calls": planned.len(),
        "ran": ran,
        "failed": failed,
        "undo_steps": undo_steps,
        "results": entries,
    });
    if let Some(note) = note {
        summary["note"] = json!(note);
    }
    let mut content = vec![Content::Text(crate::mcp::json_text(&summary))];
    content.extend(pictures.into_iter().map(Content::Png));
    Ok(ToolResult {
        content,
        is_error: failed > 0 && on_error != OnError::Continue,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::{json, Value};

    use super::super::call;
    use super::*;
    use crate::scene::Owner;

    fn shared() -> Mutex<Editor> {
        let mut editor = Editor::default();
        editor.new_map();
        Mutex::new(editor)
    }

    fn batch(shared: &Mutex<Editor>, args: Value) -> (Value, ToolResult) {
        let result = run(shared, &args, &mut |name, arguments| call(shared, name, arguments));
        let text = match &result.content[0] {
            Content::Text(text) => text.clone(),
            Content::Png(_) => panic!("a batch answers with text first"),
        };
        (serde_json::from_str(&text).unwrap_or(Value::String(text)), result)
    }

    fn block(x: i32) -> Value {
        json!({ "tool": "create_block", "arguments": { "min": [x, 0, 0], "max": [x + 32, 32, 32] } })
    }

    #[test]
    fn the_calls_run_in_order_and_undo_as_one_step() {
        let shared = shared();
        let calls = json!([
            block(0),
            block(64),
            { "tool": "select", "arguments": { "solids": [] } },
            { "tool": "create_entity", "arguments": { "classname": "info_target", "origin": [0, 0, 64] } },
            { "tool": "get_selection" }
        ]);
        let (summary, result) = batch(&shared, json!({ "calls": calls }));
        assert!(!result.is_error);
        assert_eq!((summary["calls"].as_u64(), summary["ran"].as_u64()), (Some(5), Some(5)));
        assert_eq!(summary["failed"], 0);
        assert_eq!(summary["undo_steps"], 1);
        assert_eq!(summary["results"].as_array().unwrap().len(), 5);
        assert!(
            summary["results"][0]["result"]["solid"].is_u64(),
            "answers are embedded as JSON"
        );
        assert_eq!(summary["results"][4]["result"]["entities"].as_array().unwrap().len(), 1);
        let mut editor = editor::lock(&shared);
        assert_eq!((editor.scene.solid_count, editor.scene.entity_count), (2, 1));
        assert!(editor.step_history(false).unwrap());
        assert_eq!(
            (editor.scene.solid_count, editor.scene.entity_count),
            (0, 0),
            "one undo takes it all back"
        );
        assert!(!editor.step_history(false).unwrap());
    }

    #[test]
    fn a_failing_call_stops_the_batch_or_not_as_asked() {
        let shared = shared();
        let calls = json!([block(0), { "tool": "get_entity", "arguments": { "id": 9999 } }, block(64)]);
        let (stopped, result) = batch(&shared, json!({ "calls": calls.clone() }));
        assert!(result.is_error);
        assert_eq!(
            (stopped["ran"].as_u64(), stopped["failed"].as_u64()),
            (Some(2), Some(1))
        );
        assert!(stopped["results"][1]["error"]
            .as_str()
            .unwrap()
            .contains("no entity has id 9999"));
        assert_eq!(
            editor::lock(&shared).scene.solid_count,
            1,
            "what ran before the failure stays"
        );

        let (all, result) = batch(
            &shared,
            json!({ "calls": calls.clone(), "on_error": "continue", "results": "errors" }),
        );
        assert!(!result.is_error);
        assert_eq!((all["ran"].as_u64(), all["failed"].as_u64()), (Some(3), Some(1)));
        assert_eq!(
            all["results"].as_array().unwrap().len(),
            1,
            "only the failed call is reported"
        );
        assert_eq!(all["results"][0]["call"], 1);
        assert_eq!(editor::lock(&shared).scene.solid_count, 3);

        let (undone, result) = batch(&shared, json!({ "calls": calls, "on_error": "undo" }));
        assert!(result.is_error);
        assert_eq!(undone["undo_steps"], 0);
        assert!(undone["note"].as_str().unwrap().contains("were undone"));
        assert_eq!(
            editor::lock(&shared).scene.solid_count,
            3,
            "the batch left nothing behind"
        );
    }

    #[test]
    fn only_the_last_answer_comes_back_when_asked() {
        let shared = shared();
        let calls = json!([block(0), block(64), { "tool": "get_status" }]);
        let (summary, _) = batch(&shared, json!({ "calls": calls, "results": "last" }));
        let results = summary["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["tool"], "get_status");
        assert_eq!(results[0]["result"]["solids"], 2);
        assert_eq!(summary["undo_steps"], 1);
    }

    #[test]
    fn pictures_come_back_whichever_answers_are_asked_for() {
        let shared = shared();
        let calls = json!([block(0), { "tool": "screenshot", "arguments": { "viewport": "top" } }, { "tool": "get_selection" }]);
        let (summary, result) = batch(&shared, json!({ "calls": calls, "results": "last" }));
        let results = summary["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0], json!({ "call": 1, "picture": 1, "tool": "screenshot" }));
        assert_eq!(results[1]["tool"], "get_selection");
        let pictures = result
            .content
            .iter()
            .filter(|content| matches!(content, Content::Png(_)))
            .count();
        assert_eq!(pictures, 1);
    }

    #[test]
    fn steps_stay_apart_when_someone_else_edits_in_between() {
        let shared = shared();
        let mut interrupted = false;
        let calls = json!({ "calls": [block(0), block(64)] });
        let result = run(&shared, &calls, &mut |name, arguments| {
            if std::mem::replace(&mut interrupted, true) {
                let mut editor = editor::lock(&shared);
                editor.config.agents.own_view = false;
                let solid = editor
                    .create_block(glam::DVec3::splat(512.0), glam::DVec3::splat(544.0))
                    .unwrap();
                editor.select([Owner::Solid(solid)], crate::editor::SelectMode::Remove);
                editor.config.agents.own_view = true;
            }
            call(&shared, name, arguments)
        });
        let Content::Text(text) = &result.content[0] else {
            panic!("a batch answers with text first");
        };
        let summary: Value = serde_json::from_str(text).unwrap();
        assert_eq!(summary["undo_steps"], 2);
        assert!(summary["note"].as_str().unwrap().contains("stay 2 undo steps"));
        let mut editor = editor::lock(&shared);
        assert_eq!(editor.scene.solid_count, 3);
        assert!(editor.step_history(false).unwrap());
        assert_eq!(editor.scene.solid_count, 2, "the user's own step was not swallowed");
    }

    #[test]
    fn badly_formed_batches_are_refused_whole() {
        let shared = shared();
        let nested = json!({ "calls": [block(0), { "tool": "batch", "arguments": { "calls": [] } }] });
        let (message, result) = batch(&shared, nested);
        assert!(result.is_error);
        assert!(message.as_str().unwrap().contains("cannot hold another batch"));
        assert_eq!(editor::lock(&shared).scene.solid_count, 0, "nothing ran");
        let (message, _) = batch(&shared, json!({ "calls": [] }));
        assert!(message.as_str().unwrap().contains("must be a list of calls"));
        let (message, _) = batch(&shared, json!({ "calls": [block(0)], "on_error": "retry" }));
        assert!(message.as_str().unwrap().contains("stop, continue or undo"));
    }
}
