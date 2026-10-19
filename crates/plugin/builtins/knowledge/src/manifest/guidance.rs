//! Owns readable agent workflow guidance in Knowledge's signed descriptions.
//! Manifest builders use this table; artifact storage/transfer remain their owners' internals.

const CREATE_GUIDANCE: &str = "Create a readable project-scoped artifact after search_knowledge and get_knowledge confirm no existing record to update. Supply current project_root matching the narrowest existing semantic_element_id. Use plain titles/content. Write human-readable Markdown for someone who did not follow the agent conversation: lead with the purpose or outcome, use plain language, complete sentences and short paragraphs, and add descriptive headings, lists or tables when useful. Separate paragraphs, headings and lists with blank lines. Explain why decisions matter; keep raw IDs in metadata or labeled links, and fence code/log excerpts. Do not use terse agent shorthand. For tasks use task_assignment or issue, recording status, owner, acceptance criteria, blockers and evidence when relevant. Use plan for intended steps toward a goal, checkpoint for handoffs and resumable work state (completed work, exact validation, limitations, next action) and question for an open question. Optional expires_at (RFC 3339) ends discovery of an active record. A new record starts active at revision 1. These are artifact conventions, not an atomic lease/claim guarantee.";
const UPDATE_GUIDANCE: &str = "Use search_knowledge/get_knowledge to locate and re-read an existing artifact before updating; supply current project_root and preserve human text. Pass expected_revision from that read; on knowledge_revision_conflict re-read, merge, retry. Close work by setting status, not only in the text: resolved when done or answered, superseded with superseded_by when replaced, archived when withdrawn. Write human-readable Markdown for someone who did not follow the agent conversation: lead with the purpose or outcome, use plain language, complete sentences and short paragraphs, and add descriptive headings, lists or tables when useful. Separate paragraphs, headings and lists with blank lines. Explain why decisions matter; keep raw IDs in metadata or labeled links, and fence code/log excerpts. Do not use terse agent shorthand. Supplied content, tags, dependencies and metadata replace prior values: merge deliberately before writing. Mark completion only after acceptance criteria and validation are verified; record exact commands/results and remaining limitations. expected_revision detects a stale read; updates provide no atomic lease/claim guarantee; coordinate ownership outside this write.";
const GET_GUIDANCE: &str = "Read the full artifact by artifact_id after search/list discovery. This ID-only request has no project_root field: verify the returned artifact's project_root and semantic anchor match the current checkout. The record carries status, revision and timestamps; a record that is not active is history, not a current instruction. Restore tasks/checkpoints/handoffs by reconciling their status, owner, blockers and evidence with current source/tests before resuming; saved completion text alone is not verification.";
const SEARCH_GUIDANCE: &str = "Search readable project knowledge with explicit current project_root and optional narrow semantic_element_id/type filters before creating or changing a record. Only active records are returned by default; include_inactive: true adds resolved, superseded, archived and expired ones. Results contain IDs/titles/scores/status, not full content: follow with get_knowledge to restore instructions, tasks or checkpoints. Inspect vector/lexical mode; no result is not proof that related knowledge never existed.";
const LIST_GUIDANCE: &str = "Read artifacts attached to one existing semantic_element_id from the current project. This ID-only request has no project_root field: verify each returned artifact's project_root. Only active records are returned by default; include_inactive: true adds resolved, superseded, archived and expired ones. Inspect nearby human instructions/tasks before editing; use get_knowledge for focused checkpoint/handoff restoration.";
const DEPENDENTS_GUIDANCE: &str = "List artifacts with explicit dependency edges to target_kind/target_id (semantic_element or artifact). Verify returned project scope and inspect affected records before a change; an empty list says nothing about undocumented dependencies.";
const FIND_GUIDANCE: &str = "Find semantic owners for a narrow artifact anchor with explicit current project_root. This delegates to compiled Semantic and requires that plugin to be enabled/available; inspect returned IDs rather than inventing an anchor.";
const ELEMENT_GUIDANCE: &str = "Read an existing semantic_element_id for anchor inspection. This ID-only request has no project_root field; use an ID already discovered within the current project. The public element view omits project_root, so retain and verify the originating discovery scope.";

const EXPORT_GUIDANCE: &[(&str, &str)] = &[
    (
        "manifest",
        "Inspect Knowledge's packaged export IDs; invoke only tools present in the enabled tool catalog. Agent workflow: search/read existing project knowledge, store readable anchored tasks/checkpoints, re-read on resume, then update completion with verified evidence. Other plugins are not enabled by this manifest.",
    ),
    ("create_knowledge", CREATE_GUIDANCE),
    ("knowledge.create", CREATE_GUIDANCE),
    ("update_knowledge", UPDATE_GUIDANCE),
    ("knowledge.update", UPDATE_GUIDANCE),
    ("get_knowledge", GET_GUIDANCE),
    ("knowledge.get", GET_GUIDANCE),
    ("search_knowledge", SEARCH_GUIDANCE),
    ("knowledge.search", SEARCH_GUIDANCE),
    ("list_knowledge_for_element", LIST_GUIDANCE),
    ("knowledge.list_for_element", LIST_GUIDANCE),
    ("list_knowledge_dependents", DEPENDENTS_GUIDANCE),
    ("knowledge.list_dependents", DEPENDENTS_GUIDANCE),
    ("find_semantic_elements", FIND_GUIDANCE),
    ("knowledge.find_elements", FIND_GUIDANCE),
    ("get_semantic_element", ELEMENT_GUIDANCE),
    ("knowledge.get_element", ELEMENT_GUIDANCE),
    (
        "list_knowledge",
        "List full artifacts with explicit current project_root and optional semantic_element_ids/type/tags/limit. Only active records are returned by default; include_inactive: true adds resolved, superseded, archived and expired ones. Review readable task status/owner/blockers and nearby instructions before creating duplicates; a result limit can omit records.",
    ),
    (
        "delete_knowledge",
        "Delete an artifact by artifact_id with explicit project_root matching its stored scope. Prefer a status change (resolved, superseded or archived) when the history should stay. Read it first and inspect dependents; deleting a scoped C4 report also forgets its refresh intent. Preserve human guidance unless removal is intended.",
    ),
    (
        "preview_knowledge_transfer",
        "Read-only preview of eligible artifact copies into explicit destination project_root. Review source_project_root, source_artifact_id, target_semantic_element_id, exact_match/simhash_distance and already_copied before selecting transfer_ids. Content fingerprints indicate similarity, not project identity or permission; indexing does not apply transfers automatically.",
    ),
    (
        "apply_knowledge_transfer",
        "Apply only reviewed transfer_ids from a fresh preview for explicit destination project_root. Revalidates current eligibility/destination scope and copies artifacts with provenance, referenced attachments and eligible structured reference remapping; source artifacts remain. Completed copies are skipped; interrupted copies may need retry, so inspect copied_artifact_ids/already_copied_artifact_ids and read results. This is not an atomic batch or lease/claim operation; preview similarity is not project identity.",
    ),
    (
        "rebuild_knowledge_vectors",
        "Rebuild search vectors for existing artifacts with explicit project_root. Inspect returned job status/vectors_rebuilt, including unavailable embedding capability; this does not regenerate source knowledge or prove task completion.",
    ),
    (
        "run_cultivation",
        "Run the requested cultivation mode within explicit project_root and optional target_id/target_path. Inspect the returned run and generated artifacts; generated summaries do not replace human acceptance criteria or verified task evidence.",
    ),
    (
        "get_cultivation_run",
        "Read a stored run by run_id and verify its project_root/mode/artifact IDs. This ID-only request has no project_root field; a recorded run is not evidence that every generated artifact or implementation task is complete.",
    ),
    (
        "ensure_c4_nucleus",
        "Ensure a scoped C4 report for explicit project_root and a target_id or target_path. refresh/retry_failed may request generation work; inspect returned pending/failed state and read generated content before relying on it.",
    ),
    (
        "debug_c4_nucleus",
        "Diagnose a scoped C4 nucleus without mutation using explicit project_root and optional target_id/target_path. Inspect functional artifacts and report state before requesting generation; diagnostics are not task completion.",
    ),
    (
        "project_knowledge",
        "Read the canonical project Knowledge projection using projectRoot and changesSince. Preserve revision/scope when consuming it; readable artifacts remain the source of task/checkpoint content.",
    ),
    (
        "http.knowledge.manifest",
        "List indexed project sourceId values for Knowledge HTTP consumers. Select the exact current project root; display names/fingerprints do not identify a project.",
    ),
    (
        "http.knowledge.setup",
        "Read Obsidian setup metadata for the selected sourceId query scope; this does not create artifacts or enable other plugins.",
    ),
    (
        "http.knowledge.export",
        "Export the selected sourceId project's Knowledge projection through an HTTP envelope. Retain project/revision context for subsequent reads.",
    ),
    (
        "http.knowledge.page",
        "Read one projected element page with sourceId and elementId/element_id query scope; do not confuse document hashes with project identity.",
    ),
    (
        "http.knowledge.sync",
        "Read versioned Obsidian projection changes from a schema-v1 JSON body using sourceId, appliedRevision and contentHashes. Reconcile the returned revision/change batch; it is projection synchronization, not a task ownership claim.",
    ),
    (
        "http.knowledge.c4_nucleus",
        "HTTP adapter for scoped C4 report generation. Supply current project_root and target_id/target_path in the JSON body, then inspect pending/failed generation before trusting the report.",
    ),
    (
        "http.knowledge.c4_action",
        "Read the scoped C4 action descriptor; obtaining its method/path/query does not execute generation.",
    ),
    (
        "http.knowledge.c4_debug",
        "Read scoped C4 diagnostics with project_root and target_id/target_path in the HTTP query without mutation.",
    ),
    (
        "http.knowledge.write",
        "HTTP create adapter: put the same readable title/content, explicit project_root and existing semantic anchor as create_knowledge in the JSON body; search/read existing records first.",
    ),
    ("http.knowledge.artifacts.create", CREATE_GUIDANCE),
    ("http.knowledge.artifacts.update", UPDATE_GUIDANCE),
    (
        "http.knowledge.artifacts.delete",
        "HTTP delete adapter: read the artifact first, then supply artifact_id and matching project_root in the JSON body. Inspect dependents before intended removal.",
    ),
    (
        "http.knowledge.resolve_target",
        "Resolve a JSON-body semantic_element_id to its public anchor view. Retain the current project discovery scope because this view omits project_root.",
    ),
    (
        "http.knowledge.events",
        "Consume Knowledge live-event pages and retain the returned cursor; this feed is not project-filtered, so verify event/artifact scope. Events prompt a fresh artifact read; they are not a task lease or authoritative completion evidence.",
    ),
    (
        "http.knowledge.projection_artifacts",
        "List stored Knowledge artifacts through the HTTP projection-artifact route. Verify each artifact's project_root before treating it as current project guidance.",
    ),
    (
        "trigger.semantic_element_upserted",
        "Runtime-delivered semantic-change trigger invalidates projections and refreshes report intents. It does not automatically apply cross-project transfers; agents should not fabricate deliveries.",
    ),
    (
        "poll_functional_artifact_generation",
        "Recurring runtime consumer stores completed generated functional artifacts. Inspect pending/completed generation state; it is not an implementation-task scheduler or atomic claim service.",
    ),
];

pub(super) fn description(id: &str) -> &'static str {
    if let Some(description) = crate::documents::description(id) {
        return description;
    }
    EXPORT_GUIDANCE
        .iter()
        .find(|(export_id, _)| *export_id == id)
        .map(|(_, guidance)| *guidance)
        .unwrap_or_else(|| {
            panic!("missing Knowledge guidance for `{id}`; expected a signed export ID")
        })
}
