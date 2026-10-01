use super::*;
use std::{
    future::Future,
    task::{Context, Waker},
    time::Instant,
};

#[tokio::test]
async fn dropping_queued_future_releases_position_and_allows_next_invocation() {
    let admission = PluginInvocationAdmission::new(4, 1);
    let exports = ExportConcurrencyRegistry::new();
    let active = invocation_context("active");
    let waiting = invocation_context("waiting");
    let duration = Duration::from_secs(1);
    let permit = admission
        .admit("plugin", "export", &active, &exports, duration)
        .unwrap();
    let mut queued =
        Box::pin(admission.admit_async("plugin", "export", &waiting, &exports, duration));
    assert!(
        queued
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    assert_eq!(admission.snapshot("plugin").unwrap().queued, 1);
    drop(queued);
    drop(permit);
    assert_eq!(admission.snapshot("plugin").unwrap().queued, 0);
    assert!(
        admission
            .admit_async("plugin", "export", &waiting, &exports, duration)
            .await
            .is_ok()
    );
}

#[test]
fn dispatch_consumes_queued_position_in_every_build_profile() {
    let mut mailbox = MailboxState::default();
    let first = mailbox.enqueue(PluginInvocationClass::Foreground);
    let second = mailbox.enqueue(PluginInvocationClass::Foreground);
    mailbox.dispatch(first, PluginInvocationClass::Foreground, "export");
    assert_eq!(mailbox.queued(), 1);
    mailbox.executing_by_export.clear();
    assert!(mailbox.can_dispatch(second, PluginInvocationClass::Foreground, "export", 1, 1));
}

fn invocation_context(id: &str) -> PluginInvocationContext {
    PluginInvocationContext::new(
        id,
        "test-owner",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(1),
    )
}
