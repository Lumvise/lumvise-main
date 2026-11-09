use super::*;
mod scenario;
use scenario::ScenarioHost;

fn start(mode: &str, operation: &str) -> Value {
    json!({"action":"start","project_root":scenario::ROOT,"operation_id":operation,"mode":mode})
}

fn continue_request(reply: &Value) -> Value {
    json!({"action":"continue","project_root":scenario::ROOT,"run_id":reply["run_id"],"expected_revision":reply["revision"]})
}

fn complete(host: &mut ScenarioHost, mut reply: Value) -> Value {
    for _ in 0..100 {
        if reply["status"] != "pending" {
            return reply;
        }
        reply = host.call(continue_request(&reply), 1).unwrap();
    }
    panic!("rebuild did not make durable progress");
}

#[test]
fn names_continue_and_start_replay_do_not_repeat_finished_elements() {
    let mut host = ScenarioHost::new();
    let first = host.call(start("names", "names-run"), 1).unwrap();
    assert_eq!(first["status"], "pending");
    assert_eq!(first["progress"]["names_completed"], 1);
    let replay = host.call(start("names", "names-run"), 10).unwrap();
    assert_eq!(first, replay);
    assert_eq!(host.embed_calls.len(), 1);
    let final_reply = complete(&mut host, first);
    assert_eq!(final_reply["status"], "complete");
    assert_eq!(final_reply["index_state"], "ready");
    assert_eq!(host.embed_calls, ["Heading", "page.html"]);
}

#[test]
fn start_digest_cursor_and_active_run_are_fenced() {
    let mut host = ScenarioHost::new();
    let first = host.call(start("names", "stable"), 1).unwrap();
    assert_eq!(
        host.call(start("text", "stable"), 1).unwrap_err().code,
        "rebuild_operation_conflict"
    );
    assert_eq!(
        host.call(start("text", "other"), 1).unwrap_err().code,
        "rebuild_in_progress"
    );
    let mut stale = continue_request(&first);
    stale["expected_revision"] = json!(0);
    assert_eq!(
        host.call(stale, 1).unwrap_err().code,
        "rebuild_revision_conflict"
    );
    assert_eq!(host.embed_calls.len(), 1);
}

#[test]
fn text_yields_inside_section_and_staged_tail_is_invisible_until_complete() {
    let mut host = ScenarioHost::new();
    let prepared = host.call(start("text", "tail-run"), 1).unwrap();
    assert_eq!(prepared["progress"]["chunks_staged"], 0);
    let staged = host.call(continue_request(&prepared), 1).unwrap();
    assert_eq!(staged["progress"]["chunks_staged"], 1);
    assert_eq!(host.visible_text_hits(), 0);
    let final_reply = complete(&mut host, staged);
    assert_eq!(final_reply["status"], "complete");
    assert_eq!(final_reply["progress"]["sections_completed"], 1);
    assert_eq!(host.visible_text_hits(), 1);
    assert!(
        host.embed_calls
            .iter()
            .any(|text| text.contains("TAILFACT"))
    );
}

#[test]
fn commit_lost_ack_reconciles_before_relational_cursor_advance() {
    let mut host = ScenarioHost::new();
    host.lose_commit_ack = true;
    let mut reply = host.call(start("text", "lost-commit"), 1).unwrap();
    let mut observed_loss = false;
    for _ in 0..100 {
        match host.call(continue_request(&reply), 1) {
            Ok(next) => reply = next,
            Err(error) => {
                assert_eq!(error.code, "rebuild_retry_required");
                observed_loss = true;
                break;
            }
        }
    }
    assert!(observed_loss);
    assert_eq!(host.visible_text_hits(), 1);
    let replay = host.call(start("text", "lost-commit"), 1).unwrap();
    assert_eq!(replay["status"], "pending");
    let final_reply = complete(&mut host, replay);
    assert_eq!(final_reply["status"], "complete");
    assert_eq!(host.commit_calls, 1);
}

#[test]
fn stage_lost_ack_resumes_without_reembedding_the_durable_chunk() {
    let mut host = ScenarioHost::new();
    host.lose_stage_ack = true;
    let prepared = host.call(start("text", "lost-stage"), 1).unwrap();
    assert_eq!(
        host.call(continue_request(&prepared), 1).unwrap_err().code,
        "rebuild_retry_required"
    );
    assert!(host.lost_stage_ack_observed);
    let begin_count_before_resume = host.begin_next_chunks.len();
    // A yielded dimension probe is transient; only the acknowledged native stage must not repeat.
    let prefix = host.embed_calls[0].clone();
    let calls_when_staged = host
        .embed_calls
        .iter()
        .filter(|text| **text == prefix)
        .count();
    let replay = host.call(start("text", "lost-stage"), 1).unwrap();
    let resumed = host.call(continue_request(&replay), 1).unwrap();
    assert_eq!(host.begin_next_chunks[begin_count_before_resume], 1);
    let final_reply = complete(&mut host, resumed);
    assert_eq!(final_reply["status"], "complete");
    assert_eq!(
        host.embed_calls
            .iter()
            .filter(|text| **text == prefix)
            .count(),
        calls_when_staged
    );
}

#[test]
fn cancel_after_saved_cursor_before_begin_is_durable_and_idempotent() {
    let mut host = ScenarioHost::new();
    let first = host.call(start("text", "cancel-before-begin"), 1).unwrap();
    let request = json!({"action":"cancel","project_root":scenario::ROOT,"run_id":first["run_id"],"expected_revision":first["revision"]});
    let canceled = host.call(request.clone(), 1).unwrap();
    assert_eq!(canceled["status"], "canceled");
    assert_eq!(host.call(request, 1).unwrap(), canceled);
    assert_eq!(host.visible_text_hits(), 0);
    assert_eq!(
        host.call(continue_request(&canceled), 1).unwrap()["status"],
        "canceled"
    );
    let next = host.call(start("names", "after-cancel"), 1).unwrap();
    assert_eq!(next["status"], "pending");
}

#[test]
fn source_and_profile_changes_conflict_without_partial_publication() {
    for change_profile in [false, true] {
        let mut host = ScenarioHost::new();
        let prepared = host.call(start("text", "changed"), 1).unwrap();
        if change_profile {
            host.profile = "changed-profile".into();
        } else {
            host.page = "<h2>Heading</h2><p>different source body</p>".into();
        }
        let reply = host.call(continue_request(&prepared), 1).unwrap();
        assert_eq!(reply["status"], "conflicted");
        assert_eq!(host.visible_text_hits(), 0);
    }
}

#[test]
fn permanent_embedding_failure_never_persists_raw_source_error() {
    let mut host = ScenarioHost::new();
    host.reject_embedding = true;
    let reply = host.call(start("text", "safe-failure"), 1).unwrap();
    assert_eq!(reply["status"], "failed");
    let stored = host.persisted_run(&reply).to_string();
    assert!(!stored.contains("private_section_body"));
    assert!(!stored.contains("TAILFACT"));
    assert_eq!(reply["failures"][0]["code"], "rebuild_step_failed");
}

#[test]
fn large_manifest_is_immutable_and_progress_cas_contains_only_small_control_state() {
    let mut host = ScenarioHost::new();
    let targets = (0..7318)
        .map(|index| state::Target {
            id: format!("element-{index}"),
            path: format!("page-{}.html", index / 20),
            kind: "function".into(),
            name_fingerprint: format!("name-fingerprint-{index}"),
            content_fingerprint: Some(format!("content-fingerprint-{index}")),
        })
        .collect::<Vec<_>>();
    let original = RebuildRun {
        project_root: scenario::ROOT.into(),
        project_id: scenario::PROJECT_A.into(),
        run_id: "large-manifest".into(),
        start_digest: "start".into(),
        revision: 1,
        mode: RebuildMode::Names,
        paths: None,
        status: RebuildStatus::Pending,
        phase: RebuildPhase::Names,
        progress: RebuildProgress::default(),
        observed_source_revision: 1,
        manifest_fingerprint: digest(&targets).unwrap(),
        targets,
        position: 0,
        identity: None,
        generation: None,
        failures: vec![],
    };
    let mut next = original.clone();
    next.position = 1;
    next.progress.names_completed = 1;
    {
        let mut context = PluginContext::for_test(&mut host);
        state::create(&mut context, &original, None).unwrap();
        state::save(&mut context, &original, &mut next).unwrap();
        assert_eq!(
            state::load(
                &mut context,
                scenario::ROOT,
                scenario::PROJECT_A,
                "large-manifest"
            )
            .unwrap()
            .targets
            .len(),
            7318
        );
    }
    let manifests = host
        .storage_payloads
        .iter()
        .filter(|input| input["operation"] == "put_row" && input["value"].is_array())
        .collect::<Vec<_>>();
    assert_eq!(manifests.len(), 1);
    assert_eq!(manifests[0]["value"].as_array().unwrap().len(), 7318);
    let staged = host
        .storage_payloads
        .iter()
        .filter(|input| input["operation"] == "stage_mutations")
        .collect::<Vec<_>>();
    assert!(!staged.is_empty());
    assert!(
        staged
            .iter()
            .all(|input| !input.to_string().contains("\"targets\""))
    );
    println!(
        "7318 targets: manifest={} bytes, largest cursor CAS={} bytes",
        manifests[0].to_string().len(),
        staged
            .iter()
            .map(|input| input.to_string().len())
            .max()
            .unwrap()
    );
    let key = storage::project_key(
        scenario::ROOT,
        "vector-rebuild-manifest",
        &[scenario::PROJECT_A, &original.manifest_fingerprint],
    );
    let mut context = PluginContext::for_test(&mut host);
    storage::put(
        &mut context,
        storage::SEARCH_INDEX_STATE,
        &key,
        &Vec::<state::Target>::new(),
    )
    .unwrap();
    assert_eq!(
        state::load(
            &mut context,
            scenario::ROOT,
            scenario::PROJECT_A,
            "large-manifest"
        )
        .unwrap_err()
        .code,
        "invalid_rebuild_manifest"
    );
}

#[test]
fn reused_root_with_new_project_uuid_cannot_replay_old_pending_or_completed_run() {
    for units in [1, 100] {
        let mut host = ScenarioHost::new();
        let old = host
            .call(start("names", "shared-operation"), units)
            .unwrap();
        assert_eq!(
            host.call(start("names", "shared-operation"), 1).unwrap(),
            old
        );
        host.project_id = scenario::PROJECT_B.into();
        let new = host.call(start("names", "shared-operation"), 1).unwrap();
        assert_ne!(old["run_id"], new["run_id"]);
        assert_eq!(new["status"], "pending");
        assert_eq!(
            host.call(continue_request(&old), 1).unwrap_err().code,
            "unknown_rebuild_run"
        );
        let cancel = json!({"action":"cancel","project_root":scenario::ROOT,"run_id":old["run_id"],"expected_revision":old["revision"]});
        assert_eq!(
            host.call(cancel, 1).unwrap_err().code,
            "unknown_rebuild_run"
        );
        assert_eq!(
            host.call(start("names", "shared-operation"), 1).unwrap(),
            new
        );
    }
}

#[test]
fn source_authority_precedes_project_identity_and_all_control_record_access() {
    let mut host = ScenarioHost::new();
    host.deny_scope = true;
    assert_eq!(
        host.call(start("names", "denied"), 1).unwrap_err().code,
        "source_scope_denied"
    );
    assert_eq!(host.capability_order, ["storage.semantic:project_snapshot"]);
    assert!(host.storage_payloads.is_empty());
    host.deny_scope = false;
    host.capability_order.clear();
    host.call(start("names", "authorized"), 1).unwrap();
    assert_eq!(
        &host.capability_order[..2],
        [
            "storage.semantic:project_snapshot",
            "storage.semantic:project_identity"
        ]
    );
}
