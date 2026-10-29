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

pub(super) fn configured_hook(saved: &Value, acknowledgement: &Value) -> Value {
    let mut hook = hook_view(saved, "ready");
    let watch = acknowledgement["document"]["links"]
        .as_array()
        .and_then(|links| links.first());
    append_settings(
        &mut hook,
        &acknowledgement["agent"],
        &acknowledgement["automation"],
        watch,
    );
    hook
}

pub(super) fn inspect_executor(context: &mut PluginContext<'_>, saved: &Value, hook: &mut Value) {
    let details = permission(
        context,
        json!({"operation":"prepare_executor", "delegation_id":saved["delegation_id"],
        "revision":saved["revision"]}),
    )
    .and_then(|_| {
        invoke_agents(
            context,
            "get_agent",
            json!({"agent_id":saved["executor_agent_id"]}),
        )
    });
    match details {
        Ok(details) => append_executor_view(hook, &details),
        Err(error) => {
            if saved["enabled"] == true && hook["status"] != "pending" {
                hook["status"] = json!("blocked");
            }
            hook["error"] = json!(error.message);
        }
    }
}

fn append_executor_view(hook: &mut Value, details: &Value) {
    let documents = details["documents"].as_array().into_iter().flatten();
    let watch = documents
        .flat_map(|document| document["links"].as_array().into_iter().flatten())
        .next();
    append_settings(hook, &details["agent"], &details["automation"], watch);
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
