//! Owns agent guidance carried by Semantic's signed descriptions.
//! Manifest builders consume this table; query behavior and schemas stay elsewhere.

const EXPORT_GUIDANCE: &[(&str, &str)] = &[
    (
        "manifest",
        "Inspect Semantic's packaged export IDs. Invoke only tools present in the enabled tool catalog; this manifest does not enable other plugins.",
    ),
    (
        "graph_providers",
        "List indexed project roots. Choose the exact projectRoot for the current checkout before project-scoped queries; display names and content fingerprints are not project identity.",
    ),
    (
        "get_semantic_tree",
        "Discover project containment with explicit project_root, then retain this project's semantic_element_id values. Agent workflow: locate the owner, inspect get_dependency_tree/trace_path and get_git_impact, read current source and check_index_coverage before editing. Verify changes with focused tests and refresh through the supported compiled project indexer; never fabricate ingest batches. This tree shows indexed structure, not proof that current source is fresh.",
    ),
    (
        "get_dependency_tree",
        "Inspect indexed dependencies, dependents, or both for an existing semantic_element_id from the current project. This ID-only request has no project_root field: verify returned project_root. Include descendants when assessing module impact; missing or cyclic branches require investigation, not assumptions of complete call resolution.",
    ),
    (
        "get_element_at_location",
        "Resolve a 1-based source line to indexed semantic owners using explicit project_root and path. Inspect candidates before choosing the narrowest anchor; indexed line ranges may lag current source.",
    ),
    (
        "search_semantic_elements",
        "Find indexed owners by semantic or lexical search with explicit project_root. Inspect mode/index_state and use returned IDs for focused tree, dependency, or source reads; an empty or stale index does not prove the code is absent.",
    ),
    (
        "rebuild_search_index",
        "Rebuild search vectors for existing indexed elements in explicit project_root. Inspect the returned index_state; this does not parse changed source or rebuild static relationships. Refresh stale source through the compiled project indexer first.",
    ),
    (
        "ingest_index_batch",
        "Persist genuine source-indexer output for explicit project_root/provider_instance_id. Use the supported compiled project indexer with compiler-derived evidence where available; never fabricate semantic IDs, ranges, fingerprints, or relationships from an agent summary. replace_paths/removed_paths replace indexed partitions; snapshot pages require consistent job/page metadata. Keep workflow tasks and human notes out of source-index batches.",
    ),
    (
        "record_index_log",
        "Record an indexer's actual progress/error evidence with explicit project_root and provider_instance_id. A recorded status is bookkeeping, not a source refresh or proof of completed ingestion.",
    ),
    (
        "latest_index_log",
        "Read latest indexing progress for explicit project_root and, when known, provider_instance_id. Check status, errors and completion evidence before relying on indexed results; timestamps alone do not establish source freshness.",
    ),
    (
        "semantic_graph",
        "Project indexed nodes/edges for explicit projectRoot, targetPath and granularity. Use includeFirstNeighbors/includeExternal when inspecting nearby impact; this renderer projection is not a complete source or compiler call graph.",
    ),
    (
        "project_element_counts",
        "Count indexed elements for explicit project_root. Use counts to diagnose missing coverage, not to infer that current checkout files were fully parsed.",
    ),
    (
        "semantic_context",
        "Read full-fidelity elements, relationships or artifacts from one indexed project snapshot with explicit project_root. Select record_kind and optionally root_element_id/include_descendants; inspect commit_version/published_at rather than treating indexed records as current source.",
    ),
    (
        "create_semantic_snapshot",
        "Start a semantic PZ snapshot for explicit project_root. Track its operation_id with semantic_snapshot_status; acceptance alone is not successful snapshot completion. This exports existing semantic state, not a source reindex.",
    ),
    (
        "semantic_snapshot_status",
        "Inspect the operation_id returned by snapshot creation for progress, completion or failure. Restore the originating project context before using the result; this ID-only request does not take project_root.",
    ),
    (
        "cancel_semantic_snapshot",
        "Request cancellation of a known snapshot operation_id. Check status afterward; cancellation is not evidence of a completed usable snapshot.",
    ),
    (
        "http_semantic_context",
        "HTTP envelope adapter for semantic_context. Put explicit project_root and record_kind in the JSON body; interpret snapshot revision and root scope as on the MCP read.",
    ),
    (
        "http_search_context",
        "HTTP envelope adapter for semantic element search. Put explicit project_root/query in the JSON body and inspect search mode/index_state.",
    ),
    (
        "http_semantic_relationship_tree",
        "HTTP envelope adapter for dependency-tree reads. Supply a semantic_element_id discovered in the current project in the JSON body and verify returned project_root.",
    ),
    (
        "trigger.semantic_element_upserted",
        "Runtime-delivered element-change trigger maintains search vectors for actual indexed records. Agents should refresh via the compiled indexer, not synthesize storage-trigger deliveries.",
    ),
    (
        "trigger.semantic_artifact_upserted",
        "Runtime-delivered artifact-change trigger maintains search vectors for existing indexed artifacts. It is not an agent task/checkpoint write interface.",
    ),
];

const ANALYSIS_GUIDANCE: &[(&str, &str)] = &[
    (
        "search_graph",
        "Start with this project's indexed symbols, then inspect tree/dependencies, trace_path and get_code_snippet before editing. For pagination retain commit_version as expected_commit_version and restart if it changes.",
    ),
    (
        "get_code_snippet",
        "Inspect index_matches_source: false or unknown means indexed ranges/fingerprints do not establish current-source agreement. Refresh with the compiled project indexer before trusting stale ownership or impact.",
    ),
    (
        "search_code",
        "Reads current worktree text within indexed paths; owner ranges remain indexed. Inspect failures/failure_count and source_revision, and restart paginated reads if expected_commit_version is rejected.",
    ),
    (
        "get_graph_schema",
        "Choose relationship_labels from observed kinds/labels before tracing impact. Static resolution is syntax-derived; runtime-origin edges are observations, not compiler proof.",
    ),
    (
        "trace_path",
        "Use inbound for callers/impact, outbound for dependencies, or both. Empty relationship_labels includes all non-containment relationships; missing edges/unknown receivers are not proof of no callers.",
    ),
    (
        "get_git_impact",
        "Set the intended base (default HEAD), inspect unindexed_paths and transitive inbound impact, then read current source and run affected tests. File-level impact is conservative and limited by index coverage.",
    ),
    (
        "get_graph_metrics",
        "Treat dead-code candidates as hypotheses: entrypoints, callbacks, macros and unresolved references may be live. Confirm with source, compiler diagnostics and tests before deletion.",
    ),
    (
        "check_index_coverage",
        "Inspect parser status, syntax recovery and unresolved/ambiguous references before trusting impact. Unknown metadata needs reindexing through the compiled project indexer; unresolved external/receiver references are not dead-code proof.",
    ),
    (
        "ingest_runtime_trace",
        "Submit only measured calls between active IDs in this project, with positive counts and a trace_id. Reusing trace_id replaces its observation set; it does not repair or replace the static source index.",
    ),
];

pub(super) fn description(id: &str) -> &'static str {
    EXPORT_GUIDANCE
        .iter()
        .find(|(export_id, _)| *export_id == id)
        .map(|(_, guidance)| *guidance)
        .unwrap_or_else(|| {
            panic!("missing Semantic guidance for `{id}`; expected a signed export ID")
        })
}

pub(crate) fn analysis_description(id: &str, existing: &str) -> String {
    let guidance = ANALYSIS_GUIDANCE
        .iter()
        .find(|(export_id, _)| *export_id == id)
        .map(|(_, guidance)| *guidance)
        .unwrap_or_else(|| {
            panic!("missing Semantic analysis guidance for `{id}`; expected a signed export ID")
        });
    format!("{existing} Use explicit project_root. {guidance}")
}
