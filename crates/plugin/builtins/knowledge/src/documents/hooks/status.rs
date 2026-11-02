//! Reads bounded configuration/status from the Agent public view; Agent storage stays private.
use super::*;

pub(super) fn setup_status(saved: &Value) -> &'static str {
    if saved["enabled"] != true {
        return "disabled";
    }
    if saved["executor_observations"]
        .as_array()
        .is_some_and(|facts| !facts.is_empty())
    {
        return "ready";
    }
    "pending"
}

/// What `configure_document_hook` returns: the host's current view of the permission with the
/// settings just requested. Agents finishes the executor after Knowledge returns, so this is usually
/// `pending`; `get_document_hook` reports the executor's own settings once it is `ready`.
pub(super) fn configured_hook(saved: &Value, settings: &Value, root: &str) -> Value {
    let mut hook = hook_view(saved, setup_status(saved));
    append_requested(&mut hook, saved, settings, root);
    hook
}

/// Shows the settings of a setup that has not been published yet, in the shape of a published hook.
fn append_requested(hook: &mut Value, saved: &Value, settings: &Value, root: &str) {
    let mut shown = json!({"objective":settings["objective"],"schedule":settings["schedule"],
        "propagation":settings["propagation"]});
    if settings["linked_inputs"].is_array() {
        shown["linked_inputs"] = settings["linked_inputs"].clone();
    }
    hook["settings"] = shown;
    hook["watch"] = json!({"target_kind":"document","project_root":root,
        "document_id":saved["document_id"],"propagation":settings["propagation"],
        "linked_inputs":settings.get("linked_inputs").filter(|inputs| inputs.is_array()),
        "pending_updates":0,"pending_since":Value::Null});
}

/// Reads the executor Agents owns, without Knowledge: Agents cannot call Knowledge from inside this
/// call. A setup Agents has not published yet shows the settings that were requested and, when its
/// publication failed, `blocked` with the reason.
pub(super) fn inspect_executor(
    context: &mut PluginContext<'_>,
    saved: &Value,
    root: &str,
    hook: &mut Value,
) {
    // Agents binds the executor as its owner before every export that names it, and only the
    // owner's own setup frame may do that.
    let details = permission(
        context,
        json!({"operation":"prepare_executor", "delegation_id":saved["delegation_id"],
        "revision":saved["revision"]}),
    )
    .and_then(|_| {
        invoke_agents(
            context,
            "read_memory_executor",
            json!({"agent_id":saved["executor_agent_id"]}),
        )
    });
    match details {
        Ok(details) => append_executor_view(hook, saved, root, &details),
        Err(error) => {
            if error.code != "plugin_invoke_cycle_denied"
                && saved["enabled"] == true
                && hook["status"] != "pending"
            {
                hook["status"] = json!("blocked");
            }
            hook["error"] = json!(error.message);
        }
    }
}

fn append_executor_view(hook: &mut Value, saved: &Value, root: &str, details: &Value) {
    let setup = &details["setup"];
    let unfinished = saved["enabled"] == true && setup["revision"] == saved["revision"];
    if unfinished {
        append_requested(hook, saved, &setup["settings"], root);
        if setup["state"] == "blocked" {
            hook["status"] = json!("blocked");
            hook["error"] = setup["error"]["message"].clone();
        }
    }
    if details["agent"].is_null() {
        return;
    }
    if !unfinished {
        let documents = details["documents"].as_array().into_iter().flatten();
        let watch = documents
            .flat_map(|document| document["links"].as_array().into_iter().flatten())
            .next();
        append_settings(hook, &details["agent"], &details["automation"], watch);
    }
    hook["state"] = details["state"].clone();
    hook["last_run"] = details["runs"]
        .as_array()
        .and_then(|runs| runs.first())
        .cloned()
        .unwrap_or(Value::Null);
}

fn append_settings(hook: &mut Value, agent: &Value, automation: &Value, watch: Option<&Value>) {
    hook["settings"] = json!({"objective":automation["mission"]["objective"],"schedule":agent["schedule"],
        "propagation":watch.map(|watch| &watch["propagation"])});
    if let Some(inputs) = watch
        .map(|watch| &watch["linked_inputs"])
        .filter(|inputs| inputs.is_array())
    {
        hook["settings"]["linked_inputs"] = inputs.clone();
    }
    hook["watch"] = json!(watch);
}
