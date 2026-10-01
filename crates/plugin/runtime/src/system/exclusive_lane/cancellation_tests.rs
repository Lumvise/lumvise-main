use super::*;
use crate::PluginInvocationClass;
use std::{
    future::Future,
    task::{Context, Waker},
};

#[test]
fn dropping_acquired_invocation_releases_only_its_lease() {
    let lanes = ExclusiveInvocationLanes::new();
    let export = super::tests::acquire_export();
    let input = super::tests::request("owner", "a");
    let acquired = lanes.admit("plugin", &export, &input).unwrap();
    lanes.admit("sibling", &export, &input).unwrap();
    let guard = lanes.guard("plugin", acquired);
    assert!(lanes.admit("plugin", &export, &input).is_err());
    drop(guard);
    assert!(lanes.admit("plugin", &export, &input).is_ok());
    assert!(lanes.admit("sibling", &export, &input).is_err());
}

#[test]
fn completed_nonterminal_invocation_preserves_session_lease() {
    let lanes = ExclusiveInvocationLanes::new();
    let export = super::tests::acquire_export();
    let input = super::tests::request("owner", "a");
    let acquired = lanes.admit("plugin", &export, &input).unwrap();
    lanes
        .guard("plugin", acquired)
        .finish(&Ok(WireOutcome::Succeeded {
            value: serde_json::json!({"session_id":"a", "phase":"countdown"}),
        }));
    assert!(lanes.admit("plugin", &export, &input).is_err());
}

#[tokio::test]
async fn dropping_queued_future_releases_lane_position() {
    let lanes = ExclusiveInvocationLanes::new();
    let export = super::tests::acquire_export();
    let active = super::tests::request("owner", "a");
    let first = lanes.admit("plugin", &export, &active).unwrap();
    let mut waiting = super::tests::request("owner", "b");
    waiting["queue"] = Value::Bool(true);
    let context = PluginInvocationContext::new(
        "waiting",
        "owner",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(1),
    );
    let mut queued = Box::pin(lanes.admit_async(
        "plugin",
        &export,
        &waiting,
        &context,
        Duration::from_secs(1),
    ));
    assert!(
        queued
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    drop(queued);
    lanes.finish(
        "plugin",
        first,
        &Ok(WireOutcome::Succeeded {
            value: serde_json::json!({"session_id":"a", "phase":"completed"}),
        }),
    );
    assert_eq!(
        lanes
            .snapshot("plugin", "assistant", "owner", None)
            .unwrap()
            .queued,
        0
    );
    assert!(
        lanes
            .admit_async(
                "plugin",
                &export,
                &waiting,
                &context,
                Duration::from_secs(1)
            )
            .await
            .is_ok()
    );
}
