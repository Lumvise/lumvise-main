//! Append-only catalog of every real native-tool-call leak observed in
//! production, captured as close to verbatim as practical.
//!
//! When a new leak shape shows up (a new provider/model, a new narration
//! pattern, a new escaping quirk), add ONE test here with the actual
//! captured `content` string, not a synthetic minimal case - real
//! production traces catch escaping/structural edge cases a hand-written
//! minimal example misses. Every test in this file runs on every
//! `cargo test`, so once a shape is captured here it can never silently
//! regress.
//!
//! Every test must assert the SAFE outcome per the module's fail-closed
//! contract: either the call is correctly recovered, or detection fails
//! closed (`Some(Err(_))`) - `content` must never be treated as ordinary
//! final text (`None`) when it demonstrably contains a leaked call
//! attempt.

use super::*;

const CEREBRAS: LlmProviderKind = LlmProviderKind::Cerebras;

/// Observed 2026-07-23: a `zai-glm-4.7` model on Cerebras, after a
/// multi-paragraph narrated introduction, emitted a bare (zero-argument)
/// XML-tag call with no `<arg_key>`/`<arg_value>` pairs at all, still
/// concatenated directly onto the narration with no separator.
#[test]
fn zai_glm_leaks_a_bare_zero_arg_tool_call_after_a_narrated_introduction() {
    let content = "I've explored the Knowledge Graph to understand this project's architecture. \
        This is **Lumvise**-a modular Rust application built as a workspace with distinct crates. \
        Let me create a visual diagram to explain how it all fits together.\n\n\
        This project is an intelligent development platform that combines graph-based knowledge \
        storage, AI assistant capabilities, and a plugin system. The diagram will show you how the \
        main crates interact to create a cohesive system.<tool_call>canvas_get</tool_call>";
    let result = detect(CEREBRAS, "zai-glm-4.7", content).expect("a call was attempted");
    assert!(
        result.is_err(),
        "narration concatenated directly onto a bare call tag must fail closed, never leak the \
         narration as final text nor guess that the tag is the whole answer"
    );
}

/// Observed 2026-07-23: the same session, a later turn, narrated a
/// specific canvas layer before emitting a `canvas_apply_diff` call with
/// real `agent_message`/`base_revision`/`canvas_id`/`patch` arguments
/// (patch body trimmed here to one element; the full incident had eight -
/// irrelevant to this assertion, since detection fails at the
/// narration-prefix check before the tag body is ever parsed).
#[test]
fn zai_glm_leaks_a_canvas_apply_diff_after_narrating_a_new_layer() {
    let content = "At the top we have the Application Orchestration layer. This is where the main \
        entry points live: app-core handles the application runtime and coordination, while \
        frontend-core renders the desktop and web UI that users interact with.\n\n\
        Now let me add the plugin system layer, which is central to this architecture.\
        <tool_call>builtin_assistant__canvas_apply_diff\
        <arg_key>agent_message</arg_key><arg_value>Adding the plugin system infrastructure layer</arg_value>\
        <arg_key>base_revision</arg_key><arg_value>3</arg_value>\
        <arg_key>canvas_id</arg_key><arg_value>main</arg_value>\
        <arg_key>patch</arg_key><arg_value>[{\"op\": \"add\", \"path\": \"/document/elementsById/plugin-border\", \"value\": {\"id\": \"plugin-border\"}}]</arg_value>\
        </tool_call>";
    let result = detect(CEREBRAS, "zai-glm-4.7", content).expect("a call was attempted");
    assert!(
        result.is_err(),
        "narrated layer transition text immediately followed by a real multi-arg call must fail \
         closed, matching the shape actually delivered to the live session"
    );
}

/// Observed 2026-07-23: a live user session (canvas already at revision
/// 57 from prior successful turns) - the model narrated one short sentence
/// then emitted a `canvas_apply_diff` call adding "Dispatch Components"
/// labels, again with no separator between narration and tag (patch body
/// trimmed to one element for the same reason as above).
#[test]
fn zai_glm_leaks_a_dispatch_components_diff_mid_session() {
    let content = "Now let me add the dispatch components to the diagram.\
        <tool_call>builtin_assistant__canvas_apply_diff\
        <arg_key>base_revision</arg_key><arg_value>57</arg_value>\
        <arg_key>patch</arg_key><arg_value>[{\"op\": \"add\", \"path\": \"/elementsById/dispatch-title\", \"value\": {\"id\": \"dispatch-title\"}}]</arg_value>\
        </tool_call>";
    let result = detect(CEREBRAS, "zai-glm-4.7", content).expect("a call was attempted");
    assert!(
        result.is_err(),
        "leaked mid-session narration+call content must fail closed regardless of how much prior \
         canvas progress the session already has"
    );
}
