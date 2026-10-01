//! T7.1 end-to-end check: installing the observability stack makes request
//! instrumentation surface in the Prometheus exposition. Runs in its own test
//! binary so the global tracing subscriber / metrics recorder do not collide
//! with other unit tests.

#[test]
fn recorded_requests_appear_in_rendered_metrics() {
    lumvise_app_core::observability::init();

    lumvise_app_core::observability::record_http_request(
        "POST",
        "/api/context",
        "200 OK",
        std::time::Duration::from_millis(12),
    );
    lumvise_app_core::observability::record_http_request(
        "GET",
        "/health",
        "404 Not Found",
        std::time::Duration::from_millis(3),
    );

    let rendered = lumvise_app_core::observability::render_metrics();
    assert!(
        rendered.contains("lumvise_http_requests_total"),
        "request counter missing from exposition:\n{rendered}"
    );
    assert!(
        rendered.contains("lumvise_http_request_duration_seconds"),
        "request latency histogram missing from exposition:\n{rendered}"
    );
    assert!(
        rendered.contains("method=\"POST\""),
        "POST label missing:\n{rendered}"
    );
    assert!(
        rendered.contains("status=\"4xx\""),
        "status-class label missing:\n{rendered}"
    );
}

#[test]
fn periodic_gauge_observations_surface() {
    lumvise_app_core::observability::init();
    lumvise_app_core::observability::observe_graph_node_count(42);

    let rendered = lumvise_app_core::observability::render_metrics();
    assert!(
        rendered.contains("lumvise_graph_nodes"),
        "graph node gauge missing:\n{rendered}"
    );
}
