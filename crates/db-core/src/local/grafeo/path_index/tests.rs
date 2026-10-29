//! Candidate counts prove current lookups do not hydrate a file's retained predecessors.
use super::*;
use crate::local::grafeo::graph_store::{GraphStore, GraphTransaction};
use grafeo::Value;

fn source_node(graph: &GraphTransaction<'_>, selected: Option<bool>) -> crate::Result<NodeId> {
    let mut properties = vec![
        ("project_root", Value::from("/repo")),
        ("path", Value::from("src/main.rs")),
    ];
    if let Some(selected) = selected {
        properties.push(("revision_selected", Value::from(selected)));
    }
    graph.create_node_with_props(&["SemanticElement"], properties)
}

#[test]
fn current_path_candidates_stay_constant_as_retained_history_grows() {
    for history in [0, 10, 100] {
        let store = GraphStore::new(GrafeoDB::new_in_memory());
        let expected = store
            .write(|graph| {
                let current = source_node(graph, Some(true))?;
                let unmanaged = source_node(graph, None)?;
                for _ in 0..history {
                    source_node(graph, Some(false))?;
                }
                Ok([current, unmanaged].into_iter().collect::<BTreeSet<_>>())
            })
            .unwrap();
        store.read(|graph| {
            let mut index = ElementPathIndex::default();
            index.rebuild(graph);
            assert_eq!(
                index
                    .select("/repo", "src/")
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
                expected,
                "current lookup pulled in {history} historical candidates"
            );
            assert_eq!(index.select_location("/repo", "src/main.rs").len(), 2);
        });
    }
}

#[test]
fn revision_header_paths_keep_navigation_after_body_retirement() {
    let store = GraphStore::new(GrafeoDB::new_in_memory());
    store
        .write(|graph| {
            for _ in 0..10 {
                graph.create_node_with_props(
                    &["SemanticRevisionRow"],
                    [
                        ("revision_project_root", Value::from("/repo")),
                        ("revision_record_path", Value::from("src/main.rs")),
                        ("revision_selected", Value::from(false)),
                    ],
                )?;
            }
            Ok(())
        })
        .unwrap();
    store.read(|graph| {
        let mut index = ElementPathIndex::revision_headers();
        index.rebuild(graph);
        assert_eq!(index.select("/repo", "src/").len(), 10);
    });
}

#[test]
fn warm_current_index_follows_retirement_and_reselection() {
    let store = GraphStore::new(GrafeoDB::new_in_memory());
    let node = store.write(|graph| source_node(graph, Some(true))).unwrap();
    let mut index = ElementPathIndex::default();
    store.read(|graph| index.rebuild(graph));
    for selected in [false, true, false] {
        store
            .write(|graph| graph.set_node_property(node, "revision_selected", selected.into()))
            .unwrap();
        store.read(|graph| index.apply(graph, &HashSet::from([node.0]), &HashSet::new()));
        assert_eq!(index.select("/repo", "src/").len(), usize::from(selected));
        assert_eq!(
            index.select_location("/repo", "src/main.rs").len(),
            usize::from(selected)
        );
    }
}
