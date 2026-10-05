use super::*;
use crate::AppCore;
use std::fs;
use tempfile::TempDir;

fn credentialed_app() -> AppCore {
    let app = AppCore::in_memory().expect("in-memory app");
    app.install_bridge_credential_store(std::sync::Arc::new(std::sync::Mutex::new(
        std::collections::HashMap::new(),
    )))
    .expect("credential store");
    app.set_bridge_credential(Some("project-import-test".into()), u64::MAX)
        .expect("credential");
    app
}

fn request(body: Value) -> HttpRequest {
    HttpRequest {
        method: "POST".to_string(),
        path: PROJECT_IMPORT_ENDPOINT.to_string(),
        query: std::collections::BTreeMap::new(),
        authorization: Some("Bearer project-import-test".to_string()),
        body: serde_json::to_vec(&body).expect("body serializes"),
    }
}

fn response_body(response: &HttpResponse) -> Value {
    serde_json::from_slice(response.buffered_bytes().expect("buffered body"))
        .expect("response body is JSON")
}

/// Fixture tree: one `.rs`, one root `.md`, a gitignored file, and the
/// `.gitignore` naming it.
fn fixture_tree() -> TempDir {
    let directory = TempDir::new().unwrap();
    let root = directory.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    helper();\n}\n\nfn helper() {}\n",
    )
    .unwrap();
    fs::write(root.join("README.md"), "# Guide\n").unwrap();
    fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
    fs::write(root.join("ignored.txt"), "hidden\n").unwrap();
    directory
}

#[test]
fn rejects_missing_folder_path() {
    let app = credentialed_app();
    for body in [
        json!({}),
        json!({"folderPath": "   "}),
        json!({"folderPath": 7}),
    ] {
        let response = project_import_response(&app, &request(body));
        assert_eq!(response.status, "400 Bad Request");
    }
}

#[test]
fn refresh_captured_before_removal_cannot_recreate_a_project() {
    let app = credentialed_app();
    let directory = fixture_tree();
    let root = directory.path().canonicalize().unwrap();
    let project_root = root.to_string_lossy().into_owned();
    let response = import_project(
        &app,
        ImportInput {
            folder_path: project_root.clone(),
            project_root: Some(project_root.clone()),
            replace_paths: None,
            source: None,
        },
        true,
    )
    .ok()
    .expect("stale refresh is a successful no-op");
    assert_eq!(response["elementsUpserted"], json!(0));
    assert!(app.project_import_sessions.lock().unwrap().is_empty());
    assert!(!project_is_indexed(&app, &project_root).ok().unwrap());
    assert!(root.join("src/main.rs").is_file());
}

#[test]
fn removing_a_project_discards_acknowledged_scans_for_explicit_reimport() {
    let app = credentialed_app();
    let directory = fixture_tree();
    let root = directory.path().canonicalize().unwrap();
    let project_root = root.to_string_lossy().into_owned();
    let mut session = import_session(&root, Arc::default()).ok().unwrap();
    let scan = session.indexer.prepare(ScanScope::Full).unwrap();
    session.indexer.commit(scan).unwrap();
    app.project_import_sessions
        .lock()
        .unwrap()
        .insert((root.clone(), project_root.clone()), session);
    let mut removal = request(json!({"projectRoot": project_root}));
    removal.path = super::super::project_removal::PROJECT_REMOVAL_ENDPOINT.into();
    let response = super::super::http_router::route_http_request(&app, removal);
    assert_eq!(response.status, "200 OK");
    assert!(app.project_import_sessions.lock().unwrap().is_empty());
    // A fresh manual import must attempt publication. With no compiled
    // Semantic plugin in this fixture, that produces its explicit error.
    let response = project_import_response(&app, &request(json!({"folderPath": project_root})));
    assert_eq!(response.status, "500 Internal Server Error");
    assert!(
        response_body(&response)
            .to_string()
            .contains("ingest_index_batch unavailable")
    );
}

#[test]
fn rejects_file_instead_of_directory() {
    let app = credentialed_app();
    let directory = TempDir::new().unwrap();
    let file = directory.path().join("note.txt");
    fs::write(&file, "x").unwrap();
    let response = project_import_response(
        &app,
        &request(json!({"folderPath": file.to_string_lossy()})),
    );
    assert_eq!(response.status, "400 Bad Request");
    let body = response_body(&response);
    assert!(body.to_string().contains("is not a directory"));
}

#[test]
fn rejects_missing_credential() {
    let app = credentialed_app();
    let mut unauthenticated = request(json!({"folderPath": "/tmp"}));
    unauthenticated.authorization = None;
    assert_eq!(
        project_import_response(&app, &unauthenticated).status,
        "401 Unauthorized"
    );
}

#[test]
fn scans_fixture_and_names_ignored_paths() {
    let directory = fixture_tree();
    let root = directory.path().canonicalize().unwrap();
    let mut indexer = ProjectIndexer::new(
        FilesystemProjectSource::open(&root).unwrap(),
        ParallelTreeSitterProjectParser::new(2).unwrap(),
    );
    let scan = indexer.prepare(ScanScope::Full).unwrap();
    assert!(scan.is_full_snapshot());
    let files_scanned = scan
        .changed_files()
        .filter(|file| file.entry.kind == SourceKind::File)
        .count();
    // src/main.rs, README.md, .gitignore — `ignored.txt` is excluded.
    assert_eq!(files_scanned, 3);

    let mut projection = SemanticIndexProjection::new(&root, PROVIDER_INSTANCE_ID).unwrap();
    let batch = projection.project(&scan).unwrap();
    let elements_upserted = batch.semantic_elements.len();
    assert!(elements_upserted > 0);
    assert!(batch.semantic_elements.iter().any(|element| {
        element.path == "src/main.rs" && element.semantic_element_type == "file"
    }));
    assert!(
        batch
            .semantic_elements
            .iter()
            .any(|element| element.path == "README.md")
    );

    let skipped = collect_excluded(&root, &accepted_paths(&scan));
    let skipped_paths: BTreeSet<&str> = skipped.iter().map(|(path, _)| path.as_str()).collect();
    assert!(skipped_paths.contains("ignored.txt"));
    assert!(!skipped_paths.contains("src/main.rs"));

    // The route's response numbers come from exactly these values.
    let result = finished(
        root.to_string_lossy().as_ref(),
        files_scanned,
        elements_upserted,
        batch.semantic_relationships.len(),
        skipped,
        "started",
    );
    assert_eq!(result["filesScanned"], json!(3));
    assert_eq!(result["elementsUpserted"], json!(elements_upserted));
    let skipped = result["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0]["path"], json!("ignored.txt"));
}

#[test]
fn full_snapshot_pages_carry_no_replace_paths() {
    let directory = fixture_tree();
    let root = directory.path().canonicalize().unwrap();
    let mut indexer = ProjectIndexer::new(
        FilesystemProjectSource::open(&root).unwrap(),
        ParallelTreeSitterProjectParser::new(2).unwrap(),
    );
    let scan = indexer.prepare(ScanScope::Full).unwrap();
    let mut projection = SemanticIndexProjection::new(&root, PROVIDER_INSTANCE_ID).unwrap();
    let batch = projection.project(&scan).unwrap();
    let pages = snapshot_pages(&batch, "job", true);
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0]["replace_paths"], json!([]));
    assert_eq!(pages[0]["ingestion_page_count"], json!(1));
    assert_eq!(
        pages[0]["semantic_sources"]
            .as_array()
            .expect("source upsert")
            .len(),
        1
    );
    assert_eq!(pages[0]["ingestion_job_id"], json!("job"));
    assert_eq!(pages[0]["ingestion_page_index"], json!(0));
}

/// Publication needs the compiled `builtin.semantic` plugin, which an
/// in-memory app has no install of; the route must fail closed with a
/// message naming the missing tool instead of reporting success.
#[test]
fn publication_failure_fails_closed_when_semantic_plugin_is_absent() {
    let app = credentialed_app();
    let directory = fixture_tree();
    let response = project_import_response(
        &app,
        &request(json!({"folderPath": directory.path().to_string_lossy()})),
    );
    assert_eq!(response.status, "500 Internal Server Error");
    let body = response_body(&response);
    let message = body.to_string();
    assert!(
        message.contains("builtin.semantic/ingest_index_batch unavailable"),
        "unexpected failure message: {message}"
    );
    let mut sessions = app.project_import_sessions.lock().unwrap();
    assert_eq!(sessions.len(), 1);
    let retry = sessions
        .values_mut()
        .next()
        .unwrap()
        .indexer
        .prepare(ScanScope::Full)
        .unwrap();
    assert!(
        retry.is_full_snapshot(),
        "failed publication must remain retryable"
    );
}

#[test]
fn refresh_retry_doubles_from_one_minute_to_one_hour() {
    let now = Instant::now();
    let mut retry = None;
    let mut waits = Vec::new();
    for _ in 0..9 {
        let next = RefreshRetry::after_failure(retry, now);
        waits.push((next.not_before - now).as_secs() / 60);
        retry = Some(next);
    }
    assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
    assert_eq!(retry.unwrap().failures, 9);
}

/// A failing refresh must not retry every round: the next background
/// refresh waits for the backoff, an explicit import still runs, and the
/// refresh resumes once the wait has elapsed.
#[test]
fn failed_refresh_waits_for_backoff_while_manual_import_still_runs() {
    let app = credentialed_app();
    let directory = fixture_tree();
    let root = directory.path().canonicalize().unwrap();
    let project_root = root.to_string_lossy().into_owned();
    index_fixture_project(&app, &project_root);
    let refresh = |app: &AppCore| {
        import_project(
            app,
            ImportInput {
                folder_path: project_root.clone(),
                project_root: Some(project_root.clone()),
                replace_paths: None,
                source: None,
            },
            true,
        )
    };
    let retry = |app: &AppCore| {
        app.project_import_sessions.lock().unwrap()[&(root.clone(), project_root.clone())]
            .refresh_retry
    };

    // No compiled semantic plugin: publication fails deterministically.
    assert!(refresh(&app).is_err());
    assert_eq!(retry(&app).unwrap().failures, 1);
    let deferred = refresh(&app).ok().expect("deferred refresh is a no-op");
    assert_eq!(deferred["elementsUpserted"], json!(0));
    assert_eq!(retry(&app).unwrap().failures, 1, "deferral must not retry");

    let manual = project_import_response(&app, &request(json!({"folderPath": project_root})));
    assert_eq!(manual.status, "500 Internal Server Error");
    assert_eq!(retry(&app).unwrap().failures, 2);

    let expire = |app: &AppCore| {
        let mut sessions = app.project_import_sessions.lock().unwrap();
        let session = sessions
            .get_mut(&(root.clone(), project_root.clone()))
            .unwrap();
        session.refresh_retry.as_mut().unwrap().not_before = Instant::now();
    };
    expire(&app);
    assert!(refresh(&app).is_err());
    assert_eq!(retry(&app).unwrap().failures, 3);

    // A successful refresh (nothing left to publish) clears the backoff.
    {
        let mut sessions = app.project_import_sessions.lock().unwrap();
        let session = sessions
            .get_mut(&(root.clone(), project_root.clone()))
            .unwrap();
        let scan = session.indexer.prepare(ScanScope::Full).unwrap();
        session.indexer.commit(scan).unwrap();
    }
    expire(&app);
    assert!(refresh(&app).is_ok());
    assert_eq!(retry(&app), None);
}

fn index_fixture_project(app: &AppCore, project_root: &str) {
    app.semantic
        .execute(
            SemanticOperation::SyncStructure {
                project_root: project_root.into(),
                elements: vec![lumvise_db_core::SemanticElement {
                    project_root: project_root.into(),
                    semantic_element_id: "fixture-file".into(),
                    semantic_source_id: "fixture-source".into(),
                    path: "src/main.rs".into(),
                    element_kind: "file".into(),
                    name: "main.rs".into(),
                    parent_element_id: None,
                    content_fingerprint: None,
                    start_line: None,
                    end_line: None,
                    lifecycle: "active".into(),
                    match_evidence: None,
                    metadata: json!({}),
                }],
                relationships: vec![],
            },
            &InvocationControl::sixty_seconds(),
        )
        .expect("fixture project indexed");
}

#[test]
fn retained_import_session_detects_edits_renames_and_removals() {
    let directory = fixture_tree();
    let mut session = import_session(directory.path(), Arc::default())
        .ok()
        .unwrap();
    let initial = session.indexer.prepare(ScanScope::Full).unwrap();
    session.indexer.commit(initial).unwrap();
    let unchanged = session.indexer.prepare(ScanScope::Full).unwrap();
    assert!(!unchanged.needs_publication());
    session.indexer.commit(unchanged).unwrap();
    fs::rename(
        directory.path().join("src/main.rs"),
        directory.path().join("src/renamed.rs"),
    )
    .unwrap();
    fs::write(directory.path().join("README.md"), "# Updated guide\n").unwrap();
    let changed = session.indexer.prepare(ScanScope::Full).unwrap();
    let batch = session.projection.project(&changed).unwrap();
    assert!(!changed.is_full_snapshot());
    assert!(batch.removed_paths.contains(&"src/main.rs".to_string()));
    assert!(
        batch
            .semantic_elements
            .iter()
            .any(|element| element.path == "src/renamed.rs")
    );
    assert!(
        batch
            .semantic_elements
            .iter()
            .any(|element| element.semantic_element_name == "Updated guide")
    );
}

#[test]
fn document_setting_change_rebuilds_an_acknowledged_import_session() {
    let app = credentialed_app();
    let directory = fixture_tree();
    let root = directory.path().canonicalize().unwrap();
    let mut session = import_session(&root, Arc::default()).ok().unwrap();
    let initial = session.indexer.prepare(ScanScope::Full).unwrap();
    session.indexer.commit(initial).unwrap();
    app.project_import_sessions
        .lock()
        .unwrap()
        .insert((root.clone(), root.to_string_lossy().into_owned()), session);
    app.frontend()
        .apply_app_settings_patch(
            &lumvise_frontend_core::AppSettingsPatch::DocumentEnhancementEnabled(false),
        )
        .unwrap();
    let response = project_import_response(
        &app,
        &request(json!({"folderPath":root,"replacePaths":["README.md"]})),
    );
    assert_eq!(response.status, "500 Internal Server Error"); // No semantic plugin in this fixture.
    assert!(
        response_body(&response)
            .to_string()
            .contains("ingest_index_batch unavailable")
    );
    let mut sessions = app.project_import_sessions.lock().unwrap();
    let updated = sessions.values_mut().next().unwrap();
    assert!(!updated.document_converter.options().enhancement_enabled);
    let retry = updated.indexer.prepare(ScanScope::Full).unwrap();
    assert!(retry.is_full_snapshot());
    assert!(
        retry
            .changed_files()
            .any(|file| file.entry.path == "src/main.rs")
    );
}

#[test]
fn pz_source_without_snapshot_reports_missing_archive() {
    let app = credentialed_app();
    let directory = fixture_tree();
    let response = project_import_response(
        &app,
        &request(json!({
            "folderPath": directory.path().to_string_lossy(),
            "source": "pz",
        })),
    );
    assert_eq!(response.status, "404 Not Found");
    let message = response_body(&response).to_string();
    assert!(
        message.contains("project has no linked snapshot at") && message.contains("graph.pz"),
        "unexpected failure message: {message}"
    );
}

#[test]
fn unknown_import_source_is_rejected() {
    let app = credentialed_app();
    let directory = fixture_tree();
    let response = project_import_response(
        &app,
        &request(json!({
            "folderPath": directory.path().to_string_lossy(),
            "source": "zip",
        })),
    );
    assert_eq!(response.status, "400 Bad Request");
    let message = response_body(&response).to_string();
    assert!(
        message.contains("invalid import source `zip`") && message.contains("expected"),
        "unexpected failure message: {message}"
    );
}

/// End-to-end PZ import: folder import -> `CreatePzSnapshot` -> PZ route
/// import -> the refresh poller must leave the restored elements in place.
/// Needs the real `ImportPzSnapshot` implementation; the db-core stub
/// ("PZ import pending implementation") fails this test.
#[test]
fn pz_import_publishes_snapshot_structure_and_fails_closed_without_index_log() {
    // The in-memory app has no compiled `builtin.semantic`, so the
    // provenance log cannot be written; the route must still publish the
    // archive through DB Core and then report the missing provenance.
    let app = credentialed_app();
    let control = InvocationControl::sixty_seconds();
    app.semantic
        .execute(
            SemanticOperation::SyncStructure {
                project_root: "/seed".into(),
                elements: vec![lumvise_db_core::SemanticElement {
                    project_root: "/seed".into(),
                    semantic_element_id: "seed-file".into(),
                    semantic_source_id: "seed-source".into(),
                    path: "src/main.rs".into(),
                    element_kind: "file".into(),
                    name: "main.rs".into(),
                    parent_element_id: None,
                    content_fingerprint: None,
                    start_line: None,
                    end_line: None,
                    lifecycle: "active".into(),
                    match_evidence: None,
                    metadata: json!({}),
                }],
                relationships: vec![],
            },
            &control,
        )
        .unwrap();
    let linked = TempDir::new().unwrap();
    let archive = linked.path().join(".lv").join("graph.pz");
    fs::create_dir_all(archive.parent().unwrap()).unwrap();
    app.semantic
        .execute(
            SemanticOperation::CreatePzSnapshot {
                project_root: "/seed".into(),
                output_path: archive.to_string_lossy().into_owned(),
            },
            &control,
        )
        .unwrap();
    let project_root = linked
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let response = project_import_response(
        &app,
        &request(json!({"folderPath": project_root, "source": "pz"})),
    );

    assert_eq!(response.status, "500 Internal Server Error");
    let message = response_body(&response).to_string();
    assert!(
        message.contains("completion log failed"),
        "unexpected failure message: {message}"
    );
    match app
        .semantic
        .execute(
            SemanticOperation::ProjectElementCounts { project_root },
            &control,
        )
        .unwrap()
    {
        SemanticResult::ProjectElementCounts { total_elements, .. } => {
            assert_eq!(total_elements, 1)
        }
        other => panic!("unexpected element counts result: {other:?}"),
    }
}
