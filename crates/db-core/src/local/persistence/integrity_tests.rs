//! Real corruption is injected only inside DB Core; all recovery behavior uses the portable interface.
mod catalog;

use super::{InvocationControl, LocalPersistence, SemanticPersistence};
use crate::{
    SemanticArchive, SemanticArtifact, SemanticArtifactRevisionWrite, SemanticElement,
    SemanticOperation, SemanticRecordKind, SemanticRecordRef, SemanticResult, SemanticRevisionBody,
    SemanticRevisionIntegrityReport, SemanticRevisionPublication, SemanticRevisionRecoveryReceipt,
    SemanticRevisionRecoveryRequest, SemanticRevisionState, SemanticRevisionStructure,
    SemanticSelectionExpectation,
};
use serde_json::json;

const ROOT: &str = "/integrity";
const PROVIDER: &str = "source-parser";

struct LostBodyProject {
    directory: tempfile::TempDir,
    persistence: LocalPersistence,
    before: SemanticRevisionState,
    request: SemanticRevisionRecoveryRequest,
}

impl LostBodyProject {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let persistence = LocalPersistence::open(directory.path().join("project.db")).unwrap();
        let identity = match execute(
            &persistence,
            SemanticOperation::ProjectIdentity {
                project_root: ROOT.into(),
            },
        ) {
            SemanticResult::ProjectIdentity(identity) => identity,
            other => panic!("{other:?}; expected project identity"),
        };
        execute(
            &persistence,
            SemanticOperation::PublishRevisions(initial_publication(&identity.project_id)),
        );
        let before = history(&persistence);
        delete_source_body(&persistence);
        let report = inspection(&persistence);
        assert_eq!(report.missing_records.len(), 1);
        assert_eq!(report.missing_records[0].record.record_id, "source");
        let request = SemanticRevisionRecoveryRequest {
            operation_id: "recover-source".into(),
            project_root: ROOT.into(),
            project_id: identity.project_id,
            expected_commit_version: report.commit_version,
            provider_instance_id: PROVIDER.into(),
            missing_records: vec![SemanticRecordRef::element("source")],
        };
        Self {
            directory,
            persistence,
            before,
            request,
        }
    }

    fn recover(&self) -> SemanticRevisionRecoveryReceipt {
        match execute(
            &self.persistence,
            SemanticOperation::RecoverRevisionIntegrity(self.request.clone()),
        ) {
            SemanticResult::RevisionRecovery(receipt) => receipt,
            other => panic!("{other:?}; expected recovery receipt"),
        }
    }

    fn restart(self) -> Self {
        let Self {
            directory,
            persistence,
            before,
            request,
        } = self;
        drop(persistence);
        let persistence = LocalPersistence::open(directory.path().join("project.db")).unwrap();
        Self {
            directory,
            persistence,
            before,
            request,
        }
    }
}

fn execute(persistence: &LocalPersistence, operation: SemanticOperation) -> SemanticResult {
    persistence
        .execute(operation, &InvocationControl::sixty_seconds())
        .unwrap()
}

fn import_pz_snapshot(persistence: &LocalPersistence, input_path: &str) -> crate::PzImportResult {
    match execute(
        persistence,
        SemanticOperation::ImportPzSnapshot {
            project_root: ROOT.into(),
            input_path: input_path.into(),
        },
    ) {
        SemanticResult::PzImport(result) => result,
        other => panic!("{other:?}; expected PZ import result"),
    }
}

fn inspection(persistence: &LocalPersistence) -> SemanticRevisionIntegrityReport {
    match execute(
        persistence,
        SemanticOperation::InspectRevisionIntegrity {
            publication_operation_id: None,
            project_root: ROOT.into(),
        },
    ) {
        SemanticResult::RevisionIntegrity(report) => report,
        other => panic!("{other:?}; expected integrity report"),
    }
}

#[test]
fn integrity_inspection_distinguishes_accepted_and_absent_publications_without_bodies() {
    let fixture = LostBodyProject::new();
    let before = inspection(&fixture.persistence);
    for operation_id in ["initial-source", "never-published"] {
        let SemanticResult::RevisionIntegrity(report) = execute(
            &fixture.persistence,
            SemanticOperation::InspectRevisionIntegrity {
                project_root: ROOT.into(),
                publication_operation_id: Some(operation_id.into()),
            },
        ) else {
            panic!("expected integrity report")
        };
        assert_eq!(report.commit_version, before.commit_version);
        assert_eq!(report.missing_records, before.missing_records);
        let inspected = report.publication_inspection.unwrap();
        assert_eq!(inspected.operation_id, operation_id);
        match operation_id {
            "initial-source" => assert_eq!(
                inspected.accepted.unwrap(),
                fixture.before.accepted_operations[0]
            ),
            _ => assert_eq!(inspected.accepted, None),
        }
    }
    assert_eq!(inspection(&fixture.persistence), before);
}

#[test]
fn accepted_source_publication_replays_after_its_body_is_acknowledged_unavailable() {
    let fixture = LostBodyProject::new();
    let expected = fixture.before.accepted_operations[0].receipt.clone();
    fixture.recover();
    let committed = inspection(&fixture.persistence).commit_version;
    let SemanticResult::RevisionPublication(replay) = execute(
        &fixture.persistence,
        SemanticOperation::PublishRevisions(initial_publication(&fixture.request.project_id)),
    ) else {
        panic!("expected replayed publication receipt")
    };
    assert_eq!(
        replay,
        crate::SemanticRevisionPublicationOutcome::Accepted(expected)
    );
    assert_eq!(inspection(&fixture.persistence).commit_version, committed);
}

fn history(persistence: &LocalPersistence) -> SemanticRevisionState {
    match execute(
        persistence,
        SemanticOperation::RevisionHistory {
            project_root: ROOT.into(),
        },
    ) {
        SemanticResult::RevisionHistory(state) => state,
        other => panic!("{other:?}; expected revision history"),
    }
}

fn source_element(id: &str, bytes: &str) -> SemanticElement {
    SemanticElement {
        project_root: ROOT.into(),
        semantic_element_id: id.into(),
        semantic_source_id: id.into(),
        path: format!("{id}.rs"),
        element_kind: "function".into(),
        name: id.into(),
        parent_element_id: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        content_fingerprint: None,
        match_evidence: None,
        revision_dependencies: vec![],
        metadata: json!({"provider_instance_id":PROVIDER,"source_body":bytes}),
    }
}

fn initial_publication(project_id: &str) -> SemanticRevisionPublication {
    SemanticRevisionPublication {
        operation_id: "initial-source".into(),
        producer_base: None,
        project_root: ROOT.into(),
        project_id: project_id.into(),
        selection_id: "working".into(),
        source_context: "main".into(),
        expected: SemanticSelectionExpectation::default(),
        structure: Some(SemanticRevisionStructure {
            partition: None,
            elements: vec![
                source_element("source", "historical bytes"),
                source_element("intact", "preserve me"),
            ],
            relationships: vec![],
        }),
        artifacts: vec![SemanticArtifactRevisionWrite {
            checkpoint: false,
            media_type: "text/markdown".into(),
            artifact: SemanticArtifact {
                artifact_id: "report".into(),
                semantic_element_id: "intact".into(),
                artifact_kind: "document".into(),
                title: "Human report".into(),
                content: Some("Keep this text exactly.".into()),
                content_ref: None,
                searchable_text: None,
                content_size_bytes: None,
                dependencies: vec![],
                metadata: json!({"document":{"kind":"report"}}),
            },
        }],
        removed_artifact_ids: vec![],
        source_anchor: None,
    }
}

fn delete_source_body(persistence: &LocalPersistence) {
    persistence
        .core
        .storage_manager()
        .semantic_storage()
        .graph
        .write(|graph| {
            let nodes =
                graph.find_nodes_by_property("semantic_element_id", &grafeo::Value::from("source"));
            assert_eq!(nodes.len(), 1);
            assert!(graph.delete_node(nodes[0]));
            Ok(())
        })
        .unwrap();
}

#[test]
fn recovery_is_metadata_only_and_retry_survives_restart() {
    let fixture = LostBodyProject::new();
    let receipt = fixture.recover();
    let after = history(&fixture.persistence);
    assert_eq!(after.selections, fixture.before.selections);
    assert_eq!(after.selection_history, fixture.before.selection_history);
    assert_eq!(after.revision_links, fixture.before.revision_links);
    assert_eq!(
        after.accepted_operations,
        fixture.before.accepted_operations
    );
    assert_eq!(
        after
            .records
            .iter()
            .filter(|record| record.record.record_id != "source")
            .collect::<Vec<_>>(),
        fixture
            .before
            .records
            .iter()
            .filter(|record| record.record.record_id != "source")
            .collect::<Vec<_>>()
    );
    assert_eq!(
        receipt.unavailable_records,
        vec![SemanticRecordRef::element("source")]
    );
    assert!(inspection(&fixture.persistence).missing_records.is_empty());
    let fixture = fixture.restart();
    assert_eq!(fixture.recover(), receipt);
    assert_eq!(
        history(&fixture.persistence).commit_version,
        receipt.commit_version
    );
    assert!(
        fixture
            .persistence
            .execute(
                SemanticOperation::RevisionSelection {
                    project_root: ROOT.into(),
                    selection_id: "working".into()
                },
                &InvocationControl::sixty_seconds()
            )
            .is_err()
    );
}

#[test]
fn stale_or_extra_recovery_scope_leaves_missing_headers_unmodified() {
    let mut fixture = LostBodyProject::new();
    fixture.request.expected_commit_version -= 1;
    assert!(
        fixture
            .persistence
            .execute(
                SemanticOperation::RecoverRevisionIntegrity(fixture.request.clone()),
                &InvocationControl::sixty_seconds()
            )
            .is_err()
    );
    fixture.request.expected_commit_version += 1;
    fixture
        .request
        .missing_records
        .push(SemanticRecordRef::element("intact"));
    assert!(
        fixture
            .persistence
            .execute(
                SemanticOperation::RecoverRevisionIntegrity(fixture.request.clone()),
                &InvocationControl::sixty_seconds()
            )
            .is_err()
    );
    let report = inspection(&fixture.persistence);
    assert_eq!(report.commit_version, fixture.before.commit_version);
    assert_eq!(report.missing_records.len(), 1);
    assert!(report.missing_records[0].unavailable.is_none());
}

#[test]
fn rescan_replaces_missing_selection_member_without_recreating_old_bytes() {
    let fixture = LostBodyProject::new();
    fixture.recover();
    let state = history(&fixture.persistence);
    let unavailable = match execute(
        &fixture.persistence,
        SemanticOperation::UnavailableRevisions {
            project_root: ROOT.into(),
            selection_id: "working".into(),
        },
    ) {
        SemanticResult::UnavailableRevisions(headers) => headers,
        other => panic!("{other:?}; expected unavailable headers"),
    };
    assert_eq!(unavailable.len(), 1);
    assert_eq!(unavailable[0].record.record_id, "source");
    assert_eq!(
        unavailable[0]
            .unavailable
            .as_ref()
            .unwrap()
            .recovery_operation_id,
        "recover-source"
    );
    let mut request = initial_publication(&fixture.request.project_id);
    request.operation_id = "source-rescan".into();
    request.artifacts.clear();
    request.expected = state
        .selection("working")
        .unwrap()
        .expectation(&state.records);
    request.structure = Some(SemanticRevisionStructure {
        partition: None,
        elements: vec![
            source_element("source", "rescanned bytes"),
            source_element("intact", "preserve me"),
        ],
        relationships: vec![],
    });
    execute(
        &fixture.persistence,
        SemanticOperation::PublishRevisions(request),
    );
    let after = history(&fixture.persistence);
    assert!(
        !after
            .selection("working")
            .unwrap()
            .records
            .contains(&SemanticRecordRef::element("source"))
    );
    let resolution = after
        .resolve(&SemanticRecordRef::element("source"))
        .unwrap();
    assert!(matches!(
        resolution.body,
        SemanticRevisionBody::Unavailable(_)
    ));
    assert_eq!(resolution.successors.len(), 1);
    let snapshot = after.snapshot("working", "").unwrap().unwrap();
    assert_eq!(snapshot.elements.len(), 2);
    assert!(
        snapshot
            .elements
            .iter()
            .any(|element| element.metadata["source_body"] == "rescanned bytes")
    );
    let original_report = fixture
        .before
        .records
        .iter()
        .find(|record| record.record.kind == SemanticRecordKind::Artifact)
        .unwrap();
    assert_eq!(
        after
            .records
            .iter()
            .find(|record| record.record == original_report.record)
            .unwrap(),
        original_report
    );
    assert_eq!(
        fixture.recover().commit_version,
        fixture.before.commit_version + 1
    );
    assert_eq!(
        inspection(&fixture.persistence).commit_version,
        after.commit_version
    );
}

#[test]
fn malformed_or_duplicate_present_bodies_are_not_repairable_absence() {
    for duplicate in [false, true] {
        let fixture = LostBodyProject::new();
        let id = if duplicate { "intact" } else { "source" };
        fixture
            .persistence
            .core
            .storage_manager()
            .semantic_storage()
            .graph
            .write(|graph| {
                graph.create_node_with_props(
                    &["SemanticElement"],
                    [("semantic_element_id", grafeo::Value::from(id))],
                )?;
                Ok(())
            })
            .unwrap();
        let error = fixture
            .persistence
            .execute(
                SemanticOperation::InspectRevisionIntegrity {
                    publication_operation_id: None,
                    project_root: ROOT.into(),
                },
                &InvocationControl::sixty_seconds(),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains(id), "{error}");
        assert!(
            error.contains(if duplicate {
                "duplicate bodies"
            } else {
                "complete canonical element body"
            }),
            "{error}"
        );
        assert!(
            fixture
                .persistence
                .execute(
                    SemanticOperation::RecoverRevisionIntegrity(fixture.request.clone()),
                    &InvocationControl::sixty_seconds()
                )
                .is_err()
        );
    }
}

#[test]
fn recovery_operation_ids_cannot_be_reused_for_publication() {
    let fixture = LostBodyProject::new();
    fixture.recover();
    let state = history(&fixture.persistence);
    let mut publication = initial_publication(&fixture.request.project_id);
    publication.operation_id = fixture.request.operation_id.clone();
    publication.expected = state
        .selection("working")
        .unwrap()
        .expectation(&state.records);
    let error = fixture
        .persistence
        .execute(
            SemanticOperation::PublishRevisions(publication),
            &InvocationControl::sixty_seconds(),
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("unused by collection or recovery"),
        "{error}"
    );
    assert_eq!(
        history(&fixture.persistence).commit_version,
        state.commit_version
    );
}

#[test]
fn unavailable_header_query_does_not_load_unrelated_bodies() {
    let fixture = LostBodyProject::new();
    fixture.recover();
    fixture
        .persistence
        .core
        .storage_manager()
        .semantic_storage()
        .graph
        .write(|graph| {
            graph.create_node_with_props(
                &["SemanticElement"],
                [("semantic_element_id", grafeo::Value::from("intact"))],
            )?;
            Ok(())
        })
        .unwrap();
    let result = execute(
        &fixture.persistence,
        SemanticOperation::UnavailableRevisions {
            project_root: ROOT.into(),
            selection_id: "working".into(),
        },
    );
    let SemanticResult::UnavailableRevisions(headers) = result else {
        panic!("expected unavailable headers")
    };
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].record.record_id, "source");
    assert!(
        fixture
            .persistence
            .execute(
                SemanticOperation::RevisionHistory {
                    project_root: ROOT.into()
                },
                &InvocationControl::sixty_seconds()
            )
            .is_err()
    );
}

#[test]
fn pz_round_trip_keeps_unavailable_membership_audit_and_human_report_bytes() {
    let fixture = LostBodyProject::new();
    fixture.recover();
    let SemanticResult::ProjectArchive(archive) = execute(
        &fixture.persistence,
        SemanticOperation::ProjectArchive {
            project_root: ROOT.into(),
        },
    ) else {
        panic!("expected project archive")
    };
    let path = fixture.directory.path().join("acknowledged-history.pz");
    let control = InvocationControl::sixty_seconds();
    archive.write(&path, &control).unwrap();
    let decoded = SemanticArchive::read(&path, ROOT, &control).unwrap();
    let state = decoded.revision_state.as_ref().unwrap();
    assert_eq!(state.selections, fixture.before.selections);
    assert_eq!(state.selection_history, fixture.before.selection_history);
    assert!(matches!(
        state
            .resolve(&SemanticRecordRef::element("source"))
            .unwrap()
            .body,
        SemanticRevisionBody::Unavailable(_)
    ));
    assert_eq!(
        state.recovery_operations[0].receipt.unavailable_records,
        vec![SemanticRecordRef::element("source")]
    );
    assert_eq!(
        decoded
            .snapshot
            .elements
            .iter()
            .map(|element| element.semantic_element_id.as_str())
            .collect::<Vec<_>>(),
        vec!["intact"]
    );
    assert!(
        decoded
            .artifact_blobs
            .iter()
            .any(|blob| blob.content.as_slice() == b"Keep this text exactly.")
    );
}

#[test]
fn pz_import_keeps_unavailable_metadata_without_counting_it_as_an_element_body() {
    let fixture = LostBodyProject::new();
    fixture.recover();
    let SemanticResult::ProjectArchive(archive) = execute(
        &fixture.persistence,
        SemanticOperation::ProjectArchive {
            project_root: ROOT.into(),
        },
    ) else {
        panic!("expected project archive")
    };
    let archive_path = fixture.directory.path().join("import-unavailable.pz");
    archive
        .write(&archive_path, &InvocationControl::sixty_seconds())
        .unwrap();
    let archive_path = archive_path.to_string_lossy().into_owned();
    let imported = LocalPersistence::open(fixture.directory.path().join("imported.db")).unwrap();

    let first_import = import_pz_snapshot(&imported, &archive_path);
    assert_eq!(first_import.structure.elements_upserted, 1);
    assert_eq!(first_import.artifacts_imported, 1);
    assert_eq!(first_import.artifacts_kept, 0);

    let expected = history(&fixture.persistence);
    let actual = history(&imported);
    assert_eq!(actual.project_root, expected.project_root);
    assert_eq!(actual.project_id, expected.project_id);
    assert_eq!(actual.records, expected.records);
    assert_eq!(actual.connections, expected.connections);
    assert_eq!(actual.selections, expected.selections);
    assert_eq!(actual.selection_history, expected.selection_history);
    assert_eq!(actual.revision_links, expected.revision_links);
    assert_eq!(actual.associations, expected.associations);
    assert_eq!(actual.accepted_operations, expected.accepted_operations);
    assert_eq!(actual.retention_operations, expected.retention_operations);
    assert_eq!(actual.recovery_operations, expected.recovery_operations);
    let lost = SemanticRecordRef::element("source");
    let expected_lost = expected.resolve(&lost).unwrap();
    let actual_lost = actual.resolve(&lost).unwrap();
    assert_eq!(actual_lost, expected_lost);
    let SemanticRevisionBody::Unavailable(marker) = actual_lost.body else {
        panic!("expected exact unavailable revision marker")
    };
    assert_eq!(marker.recovery_operation_id, "recover-source");

    let SemanticResult::ProjectArchive(imported_archive) = execute(
        &imported,
        SemanticOperation::ProjectArchive {
            project_root: ROOT.into(),
        },
    ) else {
        panic!("expected imported project archive")
    };
    assert_eq!(
        imported_archive
            .snapshot
            .elements
            .iter()
            .map(|element| element.semantic_element_id.as_str())
            .collect::<Vec<_>>(),
        vec!["intact"]
    );
    let report = imported_archive
        .artifact_blobs
        .iter()
        .find(|blob| blob.artifact_id == "report")
        .expect("imported human report blob");
    assert_eq!(report.content.as_slice(), b"Keep this text exactly.");

    let second_import = import_pz_snapshot(&imported, &archive_path);
    assert_eq!(second_import.structure.elements_upserted, 0);
    assert_eq!(second_import.artifacts_imported, 0);
}

#[test]
fn malformed_header_identity_fails_inspection_and_recovery_without_a_commit() {
    let mut fixture = LostBodyProject::new();
    let identity = serde_json::to_string(&SemanticRecordRef::element("source")).unwrap();
    fixture
        .persistence
        .core
        .storage_manager()
        .semantic_storage()
        .graph
        .write(|graph| {
            let nodes = graph.find_nodes_by_property(
                "revision_record_identity",
                &grafeo::Value::from(identity.clone()),
            );
            let node = graph.get_node(nodes[0]).unwrap();
            let Some(grafeo::Value::String(payload)) = node.get_property("revision_row_json")
            else {
                panic!("expected stored header")
            };
            let mut encoded: serde_json::Value = serde_json::from_str(payload).unwrap();
            encoded["value"]["record"]["record_id"] = json!("");
            graph.set_node_property(
                nodes[0],
                "revision_row_json",
                grafeo::Value::from(serde_json::to_string(&encoded).unwrap()),
            )?;
            Ok(())
        })
        .unwrap();
    let error = fixture
        .persistence
        .execute(
            SemanticOperation::InspectRevisionIntegrity {
                publication_operation_id: None,
                project_root: ROOT.into(),
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("nonempty exact revision"), "{error}");
    fixture.request.missing_records[0].record_id.clear();
    assert!(
        fixture
            .persistence
            .execute(
                SemanticOperation::RecoverRevisionIntegrity(fixture.request),
                &InvocationControl::sixty_seconds()
            )
            .is_err()
    );
    let SemanticResult::SemanticRevision { commit_version } =
        execute(&fixture.persistence, SemanticOperation::SemanticRevision)
    else {
        panic!("expected commit version")
    };
    assert_eq!(commit_version, fixture.before.commit_version);
}

#[test]
fn a_missing_prior_recovery_audit_blocks_acknowledging_another_loss() {
    let fixture = LostBodyProject::new();
    let receipt = fixture.recover();
    fixture
        .persistence
        .core
        .storage_manager()
        .semantic_storage()
        .graph
        .write(|graph| {
            let audits = graph.find_nodes_by_property(
                "revision_row_key",
                &grafeo::Value::from(
                    serde_json::to_string(&(ROOT, "recovery", "recover-source")).unwrap(),
                ),
            );
            assert_eq!(audits.len(), 1);
            assert!(graph.delete_node(audits[0]));
            let bodies = graph
                .find_nodes_by_property("semantic_element_id", &grafeo::Value::from("intact"))
                .into_iter()
                .filter(|id| {
                    graph
                        .get_node(*id)
                        .is_some_and(|node| node.has_label("SemanticElement"))
                })
                .collect::<Vec<_>>();
            assert_eq!(bodies.len(), 1);
            assert!(graph.delete_node(bodies[0]));
            Ok(())
        })
        .unwrap();
    let request = SemanticRevisionRecoveryRequest {
        operation_id: "recover-another-loss".into(),
        expected_commit_version: receipt.commit_version,
        missing_records: vec![SemanticRecordRef::element("intact")],
        ..fixture.request.clone()
    };
    for operation in [
        SemanticOperation::InspectRevisionIntegrity {
            publication_operation_id: None,
            project_root: ROOT.into(),
        },
        SemanticOperation::RecoverRevisionIntegrity(request),
    ] {
        let error = fixture
            .persistence
            .execute(operation, &InvocationControl::sixty_seconds())
            .unwrap_err()
            .to_string();
        assert!(error.contains("recovery audit"), "{error}");
    }
    let SemanticResult::SemanticRevision { commit_version } =
        execute(&fixture.persistence, SemanticOperation::SemanticRevision)
    else {
        panic!("expected commit version")
    };
    assert_eq!(commit_version, receipt.commit_version);
}
