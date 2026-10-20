use super::*;
use lumvise_contracts::CreateKnowledgeDocumentRequest;

fn report() -> KnowledgeDocument {
    crate::documents::draft_document(CreateKnowledgeDocumentRequest {
        document_id: "weekly".into(),
        project_root: "/current".into(),
        document_kind: KnowledgeDocumentKind::Report,
        title: "Weekly / briefing".into(),
        content: "Reviewed report body".into(),
        path: Some("Company/Reports".into()),
        tags: vec![],
        references: vec![],
        metadata: json!({}),
    })
}

#[test]
fn an_unanchored_report_uses_its_document_identity_title_grouping_and_body() {
    let document = report();
    let page = project(&document);
    assert!(is_report(&document));
    assert_eq!(page["elementId"], "weekly");
    assert_eq!(page["title"], "Weekly / briefing (Report)");
    assert_eq!(page["markdown"], document.content);
    assert_eq!(page["pathHint"], "Company/Reports/Weekly - briefing.md");
    assert_eq!(
        page["sourceRefs"],
        json!([{"entityKind":"document","entityId":"weekly","path":"Company/Reports"}])
    );
    assert_eq!(page["properties"], json!({}));
    assert_eq!(page["artifacts"], json!([]));
}

#[test]
fn c4_actions_use_the_current_document_owner_after_project_remapping() {
    let mut document = report();
    document.tags = vec!["scoped-c4".into()];
    document.metadata =
        json!({"nucleus":{"project_root":"/old","target_element_id":"file:source"}});
    let page = project(&document);
    assert_eq!(page["sourceId"], "/current");
    assert_eq!(
        page["properties"]["c4Update"]["payload"]["project_root"],
        "/current"
    );
    assert!(
        page["properties"]["c4Update"]["href"]
            .as_str()
            .unwrap()
            .contains("sourceId=%2Fcurrent")
    );
}

#[test]
fn context_and_archived_or_expired_reports_are_not_browsable_report_pages() {
    let mut document = report();
    document.document_kind = KnowledgeDocumentKind::Context;
    assert!(!is_report(&document));
    document.document_kind = KnowledgeDocumentKind::Report;
    document.lifecycle.status = lumvise_contracts::KnowledgeArtifactStatusV2::Archived;
    assert!(!is_report(&document));
    document.lifecycle.status = lumvise_contracts::KnowledgeArtifactStatusV2::Active;
    document.lifecycle.expires_at = Some("2000-01-01T00:00:00Z".into());
    assert!(!is_report(&document));
}
