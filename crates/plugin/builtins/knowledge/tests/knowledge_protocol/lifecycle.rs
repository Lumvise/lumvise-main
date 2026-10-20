use super::*;

#[test]
fn signed_knowledge_package_runs_through_plugin_system_lifecycle() {
    const TARGET: &str = "knowledge-integration-host";
    let workspace = tempfile::tempdir().expect("package workspace");
    let executable = std::fs::read(env!("CARGO_BIN_EXE_lumvise-plugin-knowledge"))
        .expect("read real Knowledge binary");
    let executable_hash = hex::encode(Sha256::digest(&executable));
    let manifest = package_manifest_source(TARGET, &executable_hash);
    let executable_path = manifest.targets[TARGET].clone();
    let signing_key = SigningKey::from_bytes(&[23; 32]);
    let archive_path = workspace.path().join("builtin.knowledge.lvp");
    build_package(
        &archive_path,
        BuildPackageRequest {
            manifest,
            files: BTreeMap::from([(executable_path, executable)]),
            signing_key: &signing_key,
        },
    )
    .expect("build real Knowledge package");
    let verified = verify_package(
        &archive_path,
        &signing_key.verifying_key(),
        &HostCompatibility::new(PACKAGE_PROTOCOL_VERSION, TARGET),
    )
    .expect("verify real Knowledge package");
    let installed = install_verified_package(&verified, &workspace.path().join("installed"))
        .expect("install real Knowledge package");
    let broker = Arc::new(MemoryStorageBroker::default());
    broker.seed_projection_graph();
    broker
        .rows
        .lock()
        .expect("seed foreign namespace")
        .entry(("other.plugin".into(), "knowledge_artifacts".into()))
        .or_default()
        .insert(
            "foreign".into(),
            serde_json::json!({
                "artifact_id": "foreign", "semantic_element_id": "element-a",
                "knowledge_type": "specification", "title": "Foreign",
                "content": "Must stay isolated", "tags": [], "metadata": {}
            }),
        );
    seed_paged_knowledge(&broker);
    let system = PluginSystem::with_broker_and_sandbox(
        fast_runtime_config(),
        broker.clone(),
        Arc::new(TestSubprocessSandbox),
    );

    let semantic = signed_semantic_fixture(workspace.path(), &SigningKey::from_bytes(&[24; 32]));
    system.install(&semantic).expect("catalog Semantic fixture");
    system
        .start("builtin.semantic")
        .expect("start Semantic fixture");

    system
        .install(&installed)
        .expect("catalog Knowledge package");
    system
        .start(PLUGIN_ID)
        .expect("start packaged Knowledge binary");
    let scoped = system
        .scoped_exports("assistant_session")
        .expect("assistant-scoped exports");
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].exports.len(), 14);
    for name in [
        "knowledge.create",
        "knowledge.update",
        "document.create",
        "document.update",
    ] {
        assert!(scoped[0].exports.iter().any(|export| export.id == name));
    }
    assert!(
        scoped[0]
            .exports
            .iter()
            .all(|export| export.id.starts_with("knowledge.") || export.id.starts_with("document."))
    );
    assert!(
        system
            .scoped_exports("other_scope")
            .expect("unrelated scope")
            .is_empty()
    );
    let outcome = system
        .invoke(PLUGIN_ID, MANIFEST_EXPORT_ID, serde_json::json!({}))
        .expect("invoke packaged Knowledge command");
    assert!(matches!(
        outcome,
        WireOutcome::Succeeded { value }
            if value["plugin_id"] == PLUGIN_ID
                && value["exports"][0] == MANIFEST_EXPORT_ID
    ));

    let missing_element = system
        .invoke(
            PLUGIN_ID,
            CREATE_EXPORT_ID,
            serde_json::json!({
                "artifact_id": "missing-element", "knowledge_type": "specification",
                "title": "Missing", "content": "Element absent"
            }),
        )
        .expect_err("signed schema rejects a missing semantic element id");
    assert!(missing_element.to_string().contains("semantic_element_id"));

    let invalid_kind = system
        .invoke(
            PLUGIN_ID,
            CREATE_EXPORT_ID,
            serde_json::json!({
                "artifact_id": "invalid", "semantic_element_id": "element-a",
                "knowledge_type": "invalid_kind", "title": "Invalid", "content": "Invalid"
            }),
        )
        .expect_err("signed schema rejects an invalid Knowledge kind");
    assert!(invalid_kind.to_string().contains("invalid_kind"));

    let update_missing = system
        .invoke(
            PLUGIN_ID,
            UPDATE_EXPORT_ID,
            serde_json::json!({
                "artifact_id": "absent", "title": "Still absent"
            }),
        )
        .expect("structured missing update failure");
    assert!(matches!(update_missing, WireOutcome::Failed { .. }));

    let get_semantic_tree_before = broker.plugin_invoke_count("get_semantic_tree");
    for (artifact_id, title) in [("artifact-z", "Zeta"), ("artifact-a", "Alpha")] {
        let created = system
            .invoke(
                PLUGIN_ID,
                CREATE_EXPORT_ID,
                serde_json::json!({
                    "artifact_id": artifact_id, "semantic_element_id": "element-a",
                    "knowledge_type": "specification", "title": title,
                    "content": "Shared lexical content", "tags": ["rust"],
                    "project_root": "/project"
                }),
            )
            .expect("create Knowledge");
        assert!(
            matches!(created, WireOutcome::Succeeded { .. }),
            "create failed: {created:?}"
        );
    }

    let titled = invoke_value(
        &system,
        SEARCH_EXPORT_ID,
        serde_json::json!({"query": "Zeta", "project_root": "/project", "limit": 1}),
    );
    assert_eq!(titled["mode"], "vector");
    assert_eq!(titled["results"][0]["artifact_id"], "artifact-z");
    let cold_writes = broker.vector_write_sizes.lock().unwrap().clone();
    assert!(
        cold_writes.iter().any(|size| *size > 1),
        "vectors must use batch writes: {cold_writes:?}"
    );
    assert!(cold_writes.iter().all(|size| *size <= 64));
    let cached = invoke_value(
        &system,
        SEARCH_EXPORT_ID,
        serde_json::json!({"query": "Zeta", "project_root": "/project", "limit": 1}),
    );
    assert_eq!(cached, titled);
    assert_eq!(
        *broker.vector_write_sizes.lock().unwrap(),
        cold_writes,
        "warm search must reuse stored vectors without writes"
    );

    broker.sync_elements("/other", vec![test_element("element-other", "/other")]);
    let mismatched = system.invoke(PLUGIN_ID, CREATE_EXPORT_ID, serde_json::json!({
        "artifact_id":"wrong-project", "semantic_element_id":"element-a", "project_root":"/other",
        "knowledge_type":"specification", "title":"Invalid scope", "content":"Must not appear in either project"
    })).unwrap();
    assert!(
        matches!(mismatched, WireOutcome::Failed { error } if error.code == "knowledge_project_mismatch")
    );
    let pathed = system
        .invoke(
            PLUGIN_ID,
            "knowledge.create",
            serde_json::json!({
                "artifact_id": "artifact-path", "semantic_element_id": "element-other",
                "knowledge_type": "specification", "title": "Pathed",
                "content": "Pathed unique content", "tags": ["vault"],
                "path": "notes/deep.md", "project_root": "/other",
                "mcp_owner_id": "workspace", "plugin_id": "builtin.knowledge", "session_id": "voice-edit"
            }),
        )
        .expect("create pathed Knowledge");
    assert!(
        matches!(&pathed, WireOutcome::Succeeded { value } if value["artifact"]["path"] == "notes/deep.md"),
        "create must echo the knowledge-space path: {pathed:?}"
    );

    system.stop(PLUGIN_ID).expect("restart stop");
    system.start(PLUGIN_ID).expect("restart start");
    let persisted = system
        .invoke(
            PLUGIN_ID,
            GET_EXPORT_ID,
            serde_json::json!({"artifact_id": "artifact-a"}),
        )
        .expect("get persisted Knowledge");
    assert!(
        matches!(persisted, WireOutcome::Succeeded { value } if value["artifact"]["title"] == "Alpha")
    );
    let persisted_pathed = system
        .invoke(
            PLUGIN_ID,
            GET_EXPORT_ID,
            serde_json::json!({"artifact_id": "artifact-path"}),
        )
        .expect("get persisted pathed Knowledge");
    assert!(
        matches!(persisted_pathed, WireOutcome::Succeeded { ref value } if value["artifact"]["path"] == "notes/deep.md"),
        "knowledge-space path must survive a plugin restart: {persisted_pathed:?}"
    );
    let moved = system
        .invoke(
            PLUGIN_ID,
            "knowledge.update",
            serde_json::json!({"artifact_id": "artifact-path", "path": "decisions/moved.md",
                "mcp_owner_id": "workspace", "plugin_id": "builtin.knowledge", "session_id": "voice-edit"}),
        )
        .expect("update pathed Knowledge");
    assert!(
        matches!(&moved, WireOutcome::Succeeded { value } if value["artifact"]["path"] == "decisions/moved.md"),
        "update must replace the knowledge-space path: {moved:?}"
    );
    let project_reads = broker.semantic_operation_count("project_artifacts");
    let root_reads = broker.semantic_operation_count("project_roots");
    let scoped = invoke_value(
        &system,
        "list_knowledge",
        serde_json::json!({"project_root":"/project"}),
    );
    assert!(!scoped["artifacts"].as_array().unwrap().is_empty());
    assert!(
        scoped["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|artifact| artifact["project_root"] == "/project")
    );
    assert_eq!(
        broker.semantic_operation_count("project_artifacts"),
        project_reads + 1,
        "scoped list must read only the requested project's artifacts"
    );
    assert_eq!(
        broker.semantic_operation_count("project_roots"),
        root_reads,
        "scoped list must not enumerate unrelated projects"
    );
    let projection = system
        .invoke(
            PLUGIN_ID,
            PROJECTION_EXPORT_ID,
            serde_json::json!({"projectRoot": "/project", "changesSince": 0}),
        )
        .expect("invoke signed Knowledge projection Command");
    assert!(
        matches!(&projection, WireOutcome::Succeeded { value }
        if value["spaces"][0]["spaceId"] == "project"
            && value["elements"][0]["sourceId"] == "/project"
            && value["elements"][0]["elementId"] == "file:a"
            && value["elements"][0]["children"][0]["elementId"] == "element-a"),
        "unexpected projection outcome: {projection:?}"
    );

    broker.ensure_element("element-b", "/project");
    let updated = system
        .invoke(
            PLUGIN_ID,
            UPDATE_EXPORT_ID,
            serde_json::json!({
                "artifact_id": "artifact-a", "title": "Alpha Updated",
                "semantic_element_id": "element-b"
            }),
        )
        .expect("update persisted Knowledge");
    assert!(matches!(updated, WireOutcome::Succeeded { value }
            if value["artifact"]["title"] == "Alpha Updated"
                && value["artifact"]["semantic_element_id"] == "element-b"));

    let listed = system
        .invoke(
            PLUGIN_ID,
            LIST_EXPORT_ID,
            serde_json::json!({"semantic_element_id": "element-b"}),
        )
        .expect("list Knowledge");
    assert!(matches!(listed, WireOutcome::Succeeded { value }
        if value["artifacts"].as_array().is_some_and(|items| items.len() == 1)
            && value["artifacts"][0]["artifact_id"] == "artifact-a"));
    assert_eq!(
        broker.plugin_invoke_count("get_semantic_tree"),
        get_semantic_tree_before,
        "Knowledge create/update/list identity validation must use storage.semantic Element \
         directly, never plugin.invoke get_semantic_tree"
    );

    let searched = system
        .invoke(
            PLUGIN_ID,
            SEARCH_EXPORT_ID,
            serde_json::json!({
                "query": "rust", "project_root": "/project", "semantic_element_id": "element-a"
            }),
        )
        .expect("search Knowledge");
    assert!(matches!(searched, WireOutcome::Succeeded { value }
        if value["results"].as_array().is_some_and(|items| items.len() == 1)
            && value["results"][0]["artifact_id"] == "artifact-z"));

    let paged = system
        .invoke(
            PLUGIN_ID,
            SEARCH_EXPORT_ID,
            serde_json::json!({"query": "target", "semantic_element_id": "page-two-owner"}),
        )
        .expect("search across bounded storage pages");
    assert!(matches!(paged, WireOutcome::Succeeded { value }
        if value["results"][0]["artifact_id"] == "zzzz-page-two-target"));

    let extra_field = system
        .invoke(
            PLUGIN_ID,
            GET_EXPORT_ID,
            serde_json::json!({"artifact_id": "artifact-a", "unexpected": true}),
        )
        .expect_err("signed input schema rejects unexpected fields");
    assert!(extra_field.to_string().contains("unexpected"));

    broker.put_unrelated_graph_artifact("retired-untyped-artifact", "element-a");
    assert_extended_exports(&system, &broker);
    let vector_rows = broker.rows.lock().expect("Knowledge vector rows");
    let rebuilt = vector_rows
        .get(&(PLUGIN_ID.into(), "knowledge_vectors".into()))
        .expect("persisted Knowledge vector table");
    assert!(rebuilt.len() >= 2);
    assert!(rebuilt.contains_key("artifact-a"));
    assert!(rebuilt.contains_key("artifact-z"));
    drop(vector_rows);

    broker.deny("storage.semantic");
    let denied = system
        .invoke(
            PLUGIN_ID,
            GET_ELEMENT_EXPORT_ID,
            serde_json::json!({"semantic_element_id": "element-a"}),
        )
        .expect("permission denial is a protocol result");
    assert!(matches!(denied, WireOutcome::Failed { error } if error.code == "permission_denied"));
    broker.allow_all();

    broker.fail("storage.semantic", "storage_backend_unavailable");
    let missing_target = system
        .invoke(
            PLUGIN_ID,
            GET_ELEMENT_EXPORT_ID,
            serde_json::json!({"semantic_element_id": "element-a"}),
        )
        .expect("storage failure is a protocol result");
    assert!(
        matches!(missing_target, WireOutcome::Failed { error } if error.code == "storage_backend_unavailable")
    );
    broker.allow_all();

    broker.deny("neural.embed");
    let lexical = invoke_value(
        &system,
        SEARCH_EXPORT_ID,
        serde_json::json!({"query": "rust", "semantic_element_id": "element-a"}),
    );
    assert_eq!(lexical["mode"], "lexical");
    broker.fail("neural.embed", "host_capability_unavailable");
    let unavailable_rebuild = invoke_value(
        &system,
        REBUILD_EXPORT_ID,
        serde_json::json!({"project_root": "/project"}),
    );
    assert_eq!(unavailable_rebuild["job"]["status"], "unavailable");
    assert_eq!(unavailable_rebuild["job"]["vectors_rebuilt"], 0);
    broker.allow_all();

    broker.deny("storage.semantic");
    let storage_denied = system
        .invoke(
            PLUGIN_ID,
            GET_EXPORT_ID,
            serde_json::json!({"artifact_id": "artifact-a"}),
        )
        .expect("storage denial is a protocol result");
    assert!(
        matches!(storage_denied, WireOutcome::Failed { error } if error.code == "permission_denied")
    );
    broker.allow_all();
    system
        .stop(PLUGIN_ID)
        .expect("stop packaged Knowledge binary");
    system
        .uninstall(PLUGIN_ID)
        .expect("detach Knowledge package");
    system
        .stop("builtin.semantic")
        .expect("stop Semantic fixture");
    system
        .uninstall("builtin.semantic")
        .expect("detach Semantic fixture");
    assert!(!system.is_installed(PLUGIN_ID).expect("runtime catalog"));

    make_tree_writable(workspace.path());
}

fn assert_http_knowledge_crud_matches_direct(system: &PluginSystem) {
    let artifact_id = "artifact:http/v2";
    let create_body = serde_json::json!({
        "artifact_id": artifact_id,
        "semantic_element_id": "element-a",
        "knowledge_type": "specification",
        "title": "HTTP v2",
        "content": "String identifiers survive the HTTP envelope.",
        "project_root": "/project"
    });
    let created = invoke_value(
        system,
        HTTP_CREATE_ARTIFACT_EXPORT_ID,
        http_envelope("/api/knowledge/artifacts", create_body),
    );
    assert_eq!(created["artifact"]["artifact_id"], artifact_id);
    assert_eq!(
        created,
        invoke_value(
            system,
            GET_EXPORT_ID,
            serde_json::json!({"artifact_id": artifact_id}),
        )
    );

    let updated = invoke_value(
        system,
        HTTP_UPDATE_ARTIFACT_EXPORT_ID,
        http_envelope(
            "/api/knowledge/artifacts/update",
            serde_json::json!({"artifact_id": artifact_id, "title": "HTTP v2 updated"}),
        ),
    );
    assert_eq!(updated["artifact"]["title"], "HTTP v2 updated");
    assert_eq!(
        updated,
        invoke_value(
            system,
            GET_EXPORT_ID,
            serde_json::json!({"artifact_id": artifact_id}),
        )
    );

    let direct_not_found = system
        .invoke(
            PLUGIN_ID,
            UPDATE_EXPORT_ID,
            serde_json::json!({"artifact_id": "artifact:missing", "title": "Missing"}),
        )
        .expect("direct missing update is a structured failure");
    let http_not_found = system
        .invoke(
            PLUGIN_ID,
            HTTP_UPDATE_ARTIFACT_EXPORT_ID,
            http_envelope(
                "/api/knowledge/artifacts/update",
                serde_json::json!({"artifact_id": "artifact:missing", "title": "Missing"}),
            ),
        )
        .expect("HTTP missing update is a structured failure");
    assert!(
        matches!(direct_not_found, WireOutcome::Failed { ref error } if error.code == "knowledge_not_found")
    );
    assert!(
        matches!(http_not_found, WireOutcome::Failed { ref error } if error.code == "knowledge_not_found")
    );

    let direct_mismatch = system
        .invoke(
            PLUGIN_ID,
            DELETE_EXPORT_ID,
            serde_json::json!({"artifact_id": artifact_id, "project_root": "/other"}),
        )
        .expect("direct project mismatch is a structured failure");
    let http_mismatch = system
        .invoke(
            PLUGIN_ID,
            HTTP_DELETE_ARTIFACT_EXPORT_ID,
            http_envelope(
                "/api/knowledge/artifacts/delete",
                serde_json::json!({"artifact_id": artifact_id, "project_root": "/other"}),
            ),
        )
        .expect("HTTP project mismatch is a structured failure");
    assert!(
        matches!(direct_mismatch, WireOutcome::Failed { ref error } if error.code == "knowledge_project_mismatch")
    );
    assert!(
        matches!(http_mismatch, WireOutcome::Failed { ref error } if error.code == "knowledge_project_mismatch")
    );

    let deleted = invoke_value(
        system,
        HTTP_DELETE_ARTIFACT_EXPORT_ID,
        http_envelope(
            "/api/knowledge/artifacts/delete",
            serde_json::json!({"artifact_id": artifact_id, "project_root": "/project"}),
        ),
    );
    assert_eq!(
        deleted,
        serde_json::json!({"artifact_id": artifact_id, "deleted": true})
    );
    let direct_deleted_again = invoke_value(
        system,
        DELETE_EXPORT_ID,
        serde_json::json!({"artifact_id": artifact_id, "project_root": "/project"}),
    );
    let http_deleted_again = invoke_value(
        system,
        HTTP_DELETE_ARTIFACT_EXPORT_ID,
        http_envelope(
            "/api/knowledge/artifacts/delete",
            serde_json::json!({"artifact_id": artifact_id, "project_root": "/project"}),
        ),
    );
    assert_eq!(direct_deleted_again, http_deleted_again);
    assert_eq!(http_deleted_again["deleted"], false);
}

fn http_envelope(path: &str, body: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "method": "POST",
        "path": path,
        "path_parameters": {},
        "query": {},
        "body": body,
        "body_size_bytes": 0
    })
}

fn assert_extended_exports(system: &PluginSystem, broker: &MemoryStorageBroker) {
    let get_semantic_tree_before = broker.plugin_invoke_count("get_semantic_tree");
    assert_http_knowledge_crud_matches_direct(system);
    assert_succeeds(
        system,
        FIND_ELEMENTS_EXPORT_ID,
        serde_json::json!({"query": "Element"}),
    );
    assert_succeeds(
        system,
        GET_ELEMENT_EXPORT_ID,
        serde_json::json!({"semantic_element_id": "element-a"}),
    );
    assert_succeeds(
        system,
        ASSISTANT_FIND_ELEMENTS_EXPORT_ID,
        serde_json::json!({"query": "Element"}),
    );
    assert_succeeds(
        system,
        ASSISTANT_GET_ELEMENT_EXPORT_ID,
        serde_json::json!({"semantic_element_id": "element-a"}),
    );
    assert_succeeds(
        system,
        ASSISTANT_GET_EXPORT_ID,
        serde_json::json!({"artifact_id": "artifact-a"}),
    );
    assert_succeeds(
        system,
        ASSISTANT_LIST_EXPORT_ID,
        serde_json::json!({"semantic_element_id": "element-a"}),
    );
    assert_eq!(
        broker.plugin_invoke_count("get_semantic_tree"),
        get_semantic_tree_before,
        "Knowledge HTTP create/update and get_element/list identity exports must use \
         storage.semantic Element directly, never plugin.invoke get_semantic_tree"
    );
    assert_succeeds(
        system,
        ASSISTANT_SEARCH_EXPORT_ID,
        serde_json::json!({"query": "rust"}),
    );
    let rebuilt = invoke_value(
        system,
        REBUILD_EXPORT_ID,
        serde_json::json!({"project_root": "/project"}),
    );
    assert_eq!(rebuilt["job"]["status"], "completed");
    assert_eq!(rebuilt["job"]["vectors_rebuilt"], 2); // artifact-z and artifact-a; artifact-path belongs to /other.

    let cultivation = invoke_value(
        system,
        RUN_CULTIVATION_EXPORT_ID,
        serde_json::json!({
            "mode": "cultivate_from_scratch", "project_root": "/project"
        }),
    );
    let run_id = cultivation["run"]["run_id"].as_str().expect("run id");
    assert_succeeds(
        system,
        GET_CULTIVATION_RUN_EXPORT_ID,
        serde_json::json!({"run_id": run_id}),
    );
    assert_succeeds(
        system,
        ENSURE_C4_EXPORT_ID,
        serde_json::json!({
            "project_root": "/project", "target_id": "element-a"
        }),
    );
    let report_before = invoke_value(
        system,
        "get_document",
        serde_json::json!({"document_id": "knowledge-cultivation-report-c4-architecture-scoped-file-a", "project_root":"/project"}),
    );
    assert_succeeds(
        system,
        DEBUG_C4_EXPORT_ID,
        serde_json::json!({
            "project_root": "/project", "target_id": "element-a"
        }),
    );

    let get_input = serde_json::json!({
        "method": "GET", "path": "/api/knowledge/export",
        "path_parameters": {}, "query": {"sourceId": "/project"},
        "body": null, "body_size_bytes": 0
    });
    for export in [
        HTTP_MANIFEST_EXPORT_ID,
        HTTP_SETUP_EXPORT_ID,
        HTTP_EXPORT_EXPORT_ID,
        HTTP_PROJECTION_ARTIFACTS_EXPORT_ID,
        HTTP_C4_ACTION_EXPORT_ID,
        HTTP_EVENTS_EXPORT_ID,
    ] {
        assert_succeeds(system, export, get_input.clone());
    }
    let mut page_input = get_input.clone();
    page_input["query"]["elementId"] = serde_json::json!("file:a");
    assert_succeeds(system, HTTP_PAGE_EXPORT_ID, page_input);
    let sync_input = serde_json::json!({
        "method": "POST", "path": "/api/knowledge/sync",
        "path_parameters": {}, "query": {},
        "body": {"schemaVersion": 1, "sourceId": "/project", "appliedRevision": null,
            "contentHashes": {}}, "body_size_bytes": 90
    });
    let snapshots_before_sync = broker.semantic_operation_count("project_snapshot");
    let project_delta = invoke_value(system, HTTP_SYNC_EXPORT_ID, sync_input.clone());
    assert_eq!(project_delta["sourceId"], "/project");
    assert_eq!(project_delta["schemaVersion"], 1);
    assert_eq!(project_delta["fullReset"], true);
    assert_eq!(project_delta["changedPages"][0]["elementId"], "file:a");
    assert!(
        project_delta["generatedAt"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert_eq!(
        broker.semantic_operation_count("project_snapshot"),
        snapshots_before_sync + 1
    );
    let cached_delta = invoke_value(system, HTTP_SYNC_EXPORT_ID, sync_input);
    assert_eq!(cached_delta["syncToken"], project_delta["syncToken"]);
    assert_eq!(
        broker.semantic_operation_count("project_snapshot"),
        snapshots_before_sync + 1
    );
    assert_succeeds(
        system,
        HTTP_C4_EXPORT_ID,
        serde_json::json!({
            "method": "POST", "path": "/api/knowledge/c4-nucleus", "query": {},
            "path_parameters": {}, "body": {"project_root": "/project", "target_id": "element-a"},
            "body_size_bytes": 52
        }),
    );
    assert_succeeds(
        system,
        HTTP_C4_DEBUG_EXPORT_ID,
        serde_json::json!({
            "method": "GET", "path": "/api/knowledge/c4-debug", "path_parameters": {},
            "query": {"project_root": "/project", "target_id": "element-a"},
            "body": null, "body_size_bytes": 0
        }),
    );
    assert_succeeds(
        system,
        HTTP_WRITE_EXPORT_ID,
        serde_json::json!({
            "method": "POST", "path": "/api/knowledge/write", "path_parameters": {}, "query": {},
            "body": {"artifact_id": "artifact-http", "semantic_element_id": "element-a",
                "knowledge_type": "specification", "title": "HTTP", "content": "Written"},
            "body_size_bytes": 128
        }),
    );
    let report_after = invoke_value(
        system,
        "get_document",
        serde_json::json!({"document_id": "knowledge-cultivation-report-c4-architecture-scoped-file-a", "project_root":"/project"}),
    );
    assert_eq!(
        report_before["document"]["content"],
        report_after["document"]["content"]
    );
    let report_id = "knowledge-cultivation-report-c4-architecture-scoped-file-a";
    let wrong_project = system
        .invoke(
            PLUGIN_ID,
            "delete_document",
            serde_json::json!({"document_id": report_id, "project_root": "/other"}),
        )
        .expect("project-scoped delete failure");
    assert!(matches!(wrong_project, WireOutcome::Failed { .. }));
    let deleted = invoke_value(
        system,
        "delete_document",
        serde_json::json!({"document_id": report_id, "project_root": "/project"}),
    );
    assert_eq!(deleted["deleted"], true);
    let deleted_again = system
        .invoke(
            PLUGIN_ID,
            "delete_document",
            serde_json::json!({"document_id":report_id,"project_root":"/project"}),
        )
        .unwrap();
    assert!(
        matches!(deleted_again, WireOutcome::Failed {error} if error.code == "document_not_found")
    );
    let deletion_delta = invoke_value(
        system,
        HTTP_SYNC_EXPORT_ID,
        serde_json::json!({
            "method": "POST", "path": "/api/knowledge/sync",
            "path_parameters": {}, "query": {},
            "body": {"schemaVersion": 1, "sourceId": "/project",
                "appliedRevision": project_delta["targetRevision"],
                "contentHashes": project_delta["pageHashes"]},
            "body_size_bytes": 512
        }),
    );
    assert!(
        deletion_delta["deletedPageIds"]
            .as_array()
            .expect("deleted page ids")
            .iter()
            .any(|page_id| page_id == report_id)
    );
    assert_succeeds(
        system,
        HTTP_RESOLVE_TARGET_EXPORT_ID,
        serde_json::json!({
            "method": "POST", "path": "/api/knowledge/resolve-target", "path_parameters": {}, "query": {},
            "body": {"semantic_element_id": "element-a"}, "body_size_bytes": 35
        }),
    );
    assert_succeeds(
        system,
        HTTP_C4_EXPORT_ID,
        serde_json::json!({
            "method": "POST", "path": "/api/knowledge/c4-nucleus", "query": {},
            "path_parameters": {}, "body": {"project_root": "/project", "target_id": "element-a"},
            "body_size_bytes": 52
        }),
    );
    let project_artifacts_before = broker.semantic_operation_count("project_artifacts");
    let project_roots_before = broker.semantic_operation_count("project_roots");
    let live_events_before = broker.row_count(PLUGIN_ID, "knowledge_live_events");
    let snapshots_before_trigger = broker.semantic_operation_count("project_snapshot");
    let selective_before = broker.semantic_operation_count("selective_subgraph");
    let targeted_before = broker.semantic_operation_count("elements_by_ids_including_inactive");
    let element_trigger = serde_json::json!({
        "project_root": "/project", "base_revision": 0, "target_revision": 4,
        "changed": [
            {"entity_id": "element-a", "entity_kind": "semantic_element", "disposition": "upserted"},
            {"entity_id": "element-a", "entity_kind": "semantic_element", "disposition": "upserted"},
            {"entity_id": "missing", "entity_kind": "semantic_element", "disposition": "upserted"}
        ]
    });
    assert_succeeds(system, ELEMENT_TRIGGER_EXPORT_ID, element_trigger.clone());
    assert_eq!(
        broker.semantic_operation_count("project_snapshot"),
        snapshots_before_trigger
    );
    assert_eq!(
        broker.semantic_operation_count("project_roots"),
        project_roots_before,
        "StorageTrigger inheritance must derive candidates from bounded identity keys, \
         never enumerate every semantic project root"
    );
    assert_eq!(
        broker.semantic_operation_count("project_artifacts"),
        project_artifacts_before,
        "StorageTrigger must not scan project artifacts for implicit transfer"
    );
    assert!(broker.semantic_operation_count("selective_subgraph") > selective_before);
    let live_events_after = broker.row_count(PLUGIN_ID, "knowledge_live_events");
    assert!(live_events_after > live_events_before);
    assert!(
        broker.semantic_operation_count("elements_by_ids_including_inactive") > targeted_before,
        "StorageTrigger resolves exact source, ancestor and document identities without implicit transfer"
    );
    assert_succeeds(system, ELEMENT_TRIGGER_EXPORT_ID, element_trigger.clone());
    assert_eq!(
        broker.row_count(PLUGIN_ID, "knowledge_live_events"),
        live_events_after
    );
    broker.fail("storage.semantic", "injected_failure");
    let failed = system.invoke(PLUGIN_ID, ELEMENT_TRIGGER_EXPORT_ID, element_trigger);
    assert!(matches!(failed, Ok(WireOutcome::Failed { .. })));
    broker.allow_all();
    assert_succeeds(
        system,
        ELEMENT_TRIGGER_EXPORT_ID,
        serde_json::json!({
            "project_root": "/project",
            "base_revision": 0,
            "target_revision": 3,
            "changed": (1..=3).map(|index| serde_json::json!({
                "entity_id": format!("artifact-{index}"),
                "entity_kind": "semantic_element",
                "disposition": "upserted"
            })).collect::<Vec<_>>()
        }),
    );
    let mut first_poll = get_input.clone();
    first_poll["max_events"] = serde_json::json!(2);
    let first_events = invoke_value(system, HTTP_EVENTS_EXPORT_ID, first_poll);
    assert_eq!(first_events["events"].as_array().map(Vec::len), Some(2));
    assert_eq!(first_events["events"][0]["data"]["sourceId"], "/project");
    assert_eq!(first_events["next_cursor"], "3:artifact-2");
    assert_eq!(first_events["done"], false);
    let mut second_poll = get_input;
    second_poll["cursor"] = first_events["next_cursor"].clone();
    second_poll["max_events"] = serde_json::json!(2);
    let second_events = invoke_value(system, HTTP_EVENTS_EXPORT_ID, second_poll);
    assert!(second_events["events"].is_array());
}
