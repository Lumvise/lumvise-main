use super::*;
use std::sync::mpsc;

#[test]
fn embedding_counts_are_project_scoped_and_saving_is_not_completed() {
    let activity = Arc::new(WorkspaceActivity::default());
    let mut batch = activity.track_embeddings("/repo", 50);
    batch.advance(21);
    let entry = activity
        .wait_for_changes("/repo", None, Duration::ZERO)
        .entries
        .unwrap()
        .remove(0);
    assert_eq!(
        entry.progress,
        Some(ActivityProgress {
            completed: 21,
            total: 50
        })
    );
    assert_eq!(entry.status, ActivityStatus::Running);
    assert!(
        activity
            .wait_for_changes("/other", None, Duration::ZERO)
            .entries
            .unwrap()
            .is_empty()
    );
    batch.advance(29);
    batch.saving();
    let entry = activity
        .wait_for_changes("/repo", None, Duration::ZERO)
        .entries
        .unwrap()
        .remove(0);
    assert_eq!(entry.status, ActivityStatus::Running);
    assert_eq!(entry.detail.as_deref(), Some("Saving vectors"));
    batch.succeeded();
    drop(batch);
    assert_eq!(
        activity
            .wait_for_changes("/repo", None, Duration::ZERO)
            .entries
            .unwrap()[0]
            .status,
        ActivityStatus::Succeeded
    );
}

#[test]
fn failed_embedding_keeps_real_partial_count_and_retry_replaces_only_its_terminal_row() {
    let activity = Arc::new(WorkspaceActivity::default());
    activity.record(knowledge_entry("knowledge", "/repo"));
    let mut batch = activity.track_embeddings("/repo", 50);
    batch.advance(16);
    drop(batch);
    let entries = activity
        .wait_for_changes("/repo", None, Duration::ZERO)
        .entries
        .unwrap();
    assert_eq!(entries[0].status, ActivityStatus::Failed);
    assert_eq!(entries[0].progress.as_ref().unwrap().completed, 16);
    assert_eq!(
        entries[0].detail.as_deref(),
        Some("Embedding batch failed; will retry")
    );
    let _retry = activity.track_embeddings("/repo", 20);
    let entries = activity
        .wait_for_changes("/repo", None, Duration::ZERO)
        .entries
        .unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|entry| entry.id == "knowledge"));
    assert_eq!(
        entries[0].progress,
        Some(ActivityProgress {
            completed: 0,
            total: 20
        })
    );
}

#[test]
fn embedding_progress_wakes_existing_workspace_cursor() {
    let activity = Arc::new(WorkspaceActivity::default());
    let mut batch = activity.track_embeddings("/repo", 2);
    let before = activity.wait_for_changes("/repo", None, Duration::ZERO);
    batch.advance(1);
    let after = activity.wait_for_changes("/repo", Some(before.revision), Duration::ZERO);
    assert!(after.revision > before.revision);
    assert_eq!(
        after.entries.unwrap()[0]
            .progress
            .as_ref()
            .unwrap()
            .completed,
        1
    );
}

fn knowledge_entry(id: &str, project_root: &str) -> ActivityEntry {
    ActivityEntry {
        id: id.into(),
        project_root: Some(project_root.into()),
        title: "parse · src/parser.rs".into(),
        kind: ActivityKind::Knowledge,
        status: ActivityStatus::Queued,
        semantic_element_id: Some("function:parse".into()),
        document_id: None,
        detail: None,
        progress: None,
    }
}

#[test]
fn provider_ticket_keeps_queue_running_and_completion_distinct() {
    let activity = Arc::new(WorkspaceActivity::default());
    let ticket = activity.track_llm("builtin.assistant", "fake");
    let snapshot = activity.wait_for_changes("/repo", None, Duration::ZERO);
    let entry = &snapshot.entries.unwrap()[0];
    assert_eq!(entry.kind, ActivityKind::Assistant);
    assert_eq!(entry.status, ActivityStatus::Queued);
    ticket.started();
    let snapshot = activity.wait_for_changes("/repo", None, Duration::ZERO);
    assert_eq!(snapshot.entries.unwrap()[0].status, ActivityStatus::Running);
    ticket.finish(&Ok::<(), LlmFailure>(()));
    ticket.started();
    let snapshot = activity.wait_for_changes("/repo", None, Duration::ZERO);
    assert_eq!(
        snapshot.entries.unwrap()[0].status,
        ActivityStatus::Succeeded
    );
}

#[test]
fn project_filter_includes_global_calls_and_reports_no_change_without_history() {
    let activity = Arc::new(WorkspaceActivity::default());
    activity.record(knowledge_entry("a", "/a"));
    activity.record(knowledge_entry("b", "/b"));
    let _ticket = activity.track_llm("plugin.other", "fake");
    let snapshot = activity.wait_for_changes("/a", None, Duration::ZERO);
    assert_eq!(snapshot.entries.unwrap().len(), 2);
    let unchanged = activity.wait_for_changes("/a", Some(snapshot.revision), Duration::ZERO);
    assert!(unchanged.entries.is_none());
    let reset = activity.wait_for_changes("/a", Some(u64::MAX), Duration::ZERO);
    assert_eq!(reset.entries.unwrap().len(), 2);
}

#[test]
fn waiting_reader_wakes_on_lifecycle_change() {
    let activity = Arc::new(WorkspaceActivity::default());
    let (completed, received) = mpsc::channel();
    let reader = Arc::clone(&activity);
    let waiting = std::thread::spawn(move || {
        completed
            .send(reader.wait_for_changes("/repo", Some(0), Duration::from_secs(2)))
            .unwrap();
    });
    assert!(received.recv_timeout(Duration::from_millis(30)).is_err());
    activity.record(knowledge_entry("new", "/repo"));
    let snapshot = received.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(snapshot.entries.unwrap()[0].id, "new");
    waiting.join().unwrap();
}

#[test]
fn terminal_history_is_bounded_without_evicting_active_work() {
    let activity = WorkspaceActivity::default();
    activity.record(knowledge_entry("active", "/repo"));
    for index in 0..FINISHED_HISTORY + 5 {
        let id = format!("finished-{index}");
        activity.record(knowledge_entry(&id, "/repo"));
        activity.update(&id, ActivityStatus::Succeeded, None);
    }
    let entries = activity
        .wait_for_changes("/repo", None, Duration::ZERO)
        .entries
        .unwrap();
    assert_eq!(entries.len(), FINISHED_HISTORY + 1);
    assert!(entries.iter().any(|entry| entry.id == "active"));
    assert!(!entries.iter().any(|entry| entry.id == "finished-0"));
    activity.update("active", ActivityStatus::Succeeded, None);
    let entries = activity
        .wait_for_changes("/repo", None, Duration::ZERO)
        .entries
        .unwrap();
    assert_eq!(entries.len(), FINISHED_HISTORY);
    assert_eq!(
        entries[0].id, "active",
        "a long-running call stays visible when it finishes"
    );
}

#[test]
fn failure_and_cancellation_remain_terminal() {
    let activity = Arc::new(WorkspaceActivity::default());
    let ticket = activity.track_llm("plugin.other", "fake");
    ticket.finish(&Err::<(), _>(LlmFailure::new(
        LlmFailureCode::Cancelled,
        "cancelled",
    )));
    ticket.finish(&Ok::<(), LlmFailure>(()));
    let failure = activity.track_llm("plugin.other", "fake");
    failure.finish(&Err::<(), _>(LlmFailure::new(
        LlmFailureCode::ProviderBusy,
        "full",
    )));
    let entries = activity
        .wait_for_changes("/repo", None, Duration::ZERO)
        .entries
        .unwrap();
    assert_eq!(entries[0].status, ActivityStatus::Failed);
    assert_eq!(entries[0].detail.as_deref(), Some("full"));
    assert_eq!(entries[1].status, ActivityStatus::Cancelled);
}

#[test]
fn dropping_unfinished_ticket_cancels_queued_and_running_entries() {
    let activity = Arc::new(WorkspaceActivity::default());
    let queued = activity.track_llm("app", "fake");
    let running = activity.track_llm("app", "fake");
    running.started();
    drop(queued);
    drop(running);
    let entries = activity
        .wait_for_changes("/repo", None, Duration::ZERO)
        .entries
        .unwrap();
    assert!(
        entries
            .iter()
            .all(|entry| entry.status == ActivityStatus::Cancelled)
    );
    assert!(
        entries
            .iter()
            .all(|entry| entry.detail.as_deref() == Some("abandoned before completion"))
    );
}

#[test]
fn dropping_finished_ticket_preserves_completion() {
    let activity = Arc::new(WorkspaceActivity::default());
    let ticket = activity.track_llm("app", "fake");
    ticket.finish(&Ok::<(), LlmFailure>(()));
    drop(ticket);
    let entry = &activity
        .wait_for_changes("/repo", None, Duration::ZERO)
        .entries
        .unwrap()[0];
    assert_eq!(entry.status, ActivityStatus::Succeeded);
    assert_eq!(entry.detail, None);
}

#[test]
fn start_callback_does_not_keep_ticket_alive() {
    let activity = Arc::new(WorkspaceActivity::default());
    let ticket = activity.track_llm("app", "fake");
    let _control = ticket.control(LlmExecutionControl::new(
        lumvise_resource_routing::InvocationControl::sixty_seconds(),
    ));
    drop(ticket);
    let entry = &activity
        .wait_for_changes("/repo", None, Duration::ZERO)
        .entries
        .unwrap()[0];
    assert_eq!(entry.status, ActivityStatus::Cancelled);
}
