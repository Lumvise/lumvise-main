use super::*;

#[test]
fn manifest_source_declares_storage_backed_knowledge_mcp_exports() {
    let digest = "a".repeat(64);
    let manifest = package_manifest_source("aarch64-apple-darwin", &digest);

    assert_eq!(manifest.plugin_id, PLUGIN_ID);
    assert_eq!(manifest.protocol.min, PACKAGE_PROTOCOL_VERSION);
    assert_eq!(manifest.protocol.max, PACKAGE_PROTOCOL_VERSION);
    assert_eq!(manifest.exports[0].id, MANIFEST_EXPORT_ID);
    assert!(
        !manifest
            .exports
            .iter()
            .any(|export| matches!(export.surface, ExportSurface::View { .. }))
    );
    assert_eq!(manifest.files.len(), 1);
    assert_eq!(manifest.host_capabilities[0].id, "storage.plugin");
    assert_eq!(manifest.host_capabilities[0].version, "^1.0");
    assert!(
        manifest
            .exports
            .iter()
            .any(|export| export.id == CREATE_EXPORT_ID)
    );
    assert!(
        manifest
            .exports
            .iter()
            .any(|export| export.id == DELETE_EXPORT_ID)
    );
    assert!(
        manifest
            .exports
            .iter()
            .any(|export| export.id == SEARCH_EXPORT_ID)
    );
    assert!(manifest.exports.iter().any(|export| {
        export.id == HTTP_EVENTS_EXPORT_ID
            && matches!(
                export.surface,
                lumvise_plugin_package::ExportSurface::HttpRoute {
                    stream_mode: HttpStreamMode::ServerSentEvents,
                    ..
                }
            )
    }));
    let sync = manifest
        .exports
        .iter()
        .find(|export| export.id == HTTP_SYNC_EXPORT_ID)
        .expect("Knowledge sync export");
    assert!(matches!(sync.surface, ExportSurface::HttpRoute { .. }));
    assert!(manifest.exports.iter().any(|export| {
        export.id == ELEMENT_TRIGGER_EXPORT_ID
            && matches!(&export.surface,
                ExportSurface::StorageTrigger { event_kinds, entity_kinds, .. }
                    if !event_kinds.is_empty() && entity_kinds.is_empty())
    }));
    assert!(manifest.exports.iter().all(|export| {
        export.input_schema["additionalProperties"] == false
            && export.output_schema["additionalProperties"] == false
    }));
    let global_ids = manifest
        .exports
        .iter()
        .filter(|export| {
            matches!(
                export.surface,
                lumvise_plugin_package::ExportSurface::McpTool
            )
        })
        .map(|export| export.id.as_str())
        .collect::<Vec<_>>();
    assert!(!global_ids.iter().any(|id| id.starts_with("knowledge.")));
    assert!(
        manifest
            .exports
            .iter()
            .filter(|export| {
                matches!(
                    export.surface,
                    lumvise_plugin_package::ExportSurface::ScopedMcpTool { .. }
                )
            })
            .all(|export| {
                export.input_schema["properties"]["mcp_owner_id"]["type"] == "string"
                    && export.input_schema["properties"]["plugin_id"]["type"] == "string"
                    && export.input_schema["properties"]["session_id"]["type"] == "string"
            })
    );
}

#[test]
fn real_binary_serves_declared_export_rejects_unknown_and_stops() {
    let expected_exports = package_manifest_source("test-target", &"a".repeat(64))
        .exports
        .into_iter()
        .map(|export| export.id)
        .collect::<Vec<_>>();
    let mut plugin = KnowledgePluginProcess::launch();
    let session_id = "knowledge-session";
    let package_digest = "signed-package-digest";

    let ready = plugin.exchange(MessageBody::HostHello {
        session_id: session_id.into(),
        host_id: "knowledge-test-host".into(),
        package_digest: package_digest.into(),
    });
    assert!(matches!(
        ready.body,
        MessageBody::PluginReady {
            session_id: ready_session,
            plugin_id,
            package_digest: ready_digest,
        } if ready_session == session_id && plugin_id == PLUGIN_ID && ready_digest == package_digest
    ));

    let result = plugin.exchange(MessageBody::HostInvoke {
        session_id: session_id.into(),
        invocation_id: "manifest-invocation".into(),
        capability_id: MANIFEST_EXPORT_ID.into(),
        input: serde_json::json!({}),
    });
    assert!(matches!(
        result.body,
        MessageBody::PluginResult {
            outcome: WireOutcome::Succeeded { value },
            ..
        } if value["plugin_id"] == PLUGIN_ID
            && value["protocol"] == PACKAGE_PROTOCOL_VERSION
            && value["exports"] == serde_json::json!(expected_exports)
    ));

    let unknown = plugin.exchange(MessageBody::HostInvoke {
        session_id: session_id.into(),
        invocation_id: "unknown-invocation".into(),
        capability_id: "knowledge.missing".into(),
        input: serde_json::json!({}),
    });
    assert!(matches!(
        unknown.body,
        MessageBody::PluginResult {
            outcome: WireOutcome::Failed { error },
            ..
        } if error.code == "unknown_capability" && !error.retryable
    ));

    let stopped = plugin.exchange(MessageBody::HostShutdown {
        session_id: session_id.into(),
        reason: Some("test complete".into()),
    });
    assert!(matches!(
        stopped.body,
        MessageBody::PluginStopped { session_id: stopped_session, .. }
            if stopped_session == session_id
    ));
    plugin.wait_for_success();
}
