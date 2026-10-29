use lumvise_db_core::{ArtifactTextVector, rank_vector_candidates};

fn embedding(engine: &str, model: Option<&str>, values: &[f32]) -> ArtifactTextVector {
    ArtifactTextVector {
        engine_id: engine.into(),
        model: model.map(str::to_owned),
        dimensions: values.len(),
        vector: values.into(),
        normalized: false,
        metadata: serde_json::json!({}),
    }
}

#[test]
fn shared_ranking_orders_ties_limits_and_filters_engine_model_and_dimensions() {
    let vectors = [
        ("z", embedding("engine", None, &[1.0, 0.0])),
        ("a", embedding("engine", Some(""), &[1.0, 0.0])),
        ("zero", embedding("engine", None, &[0.0, 0.0])),
        ("negative", embedding("engine", None, &[-1.0, 0.0])),
        ("wrong-engine", embedding("other", None, &[1.0, 0.0])),
        (
            "wrong-model",
            embedding("engine", Some("other"), &[1.0, 0.0]),
        ),
        ("wrong-dimensions", embedding("engine", None, &[1.0])),
    ];
    for model in [None, Some("")] {
        let ranked = rank_vector_candidates(
            "root",
            vectors.iter().map(|(id, vector)| (*id, vector)),
            &[1.0, 0.0],
            3,
            "engine",
            model,
        )
        .unwrap();
        assert_eq!(
            ranked
                .iter()
                .map(|result| (result.id.as_str(), result.score))
                .collect::<Vec<_>>(),
            [("a", 1.0), ("z", 1.0), ("zero", 0.0)]
        );
    }
}

#[test]
fn shared_ranking_validates_empty_candidates_too() {
    for (root, query, k) in [
        ("", vec![1.0], 1),
        ("root", vec![], 1),
        ("root", vec![f32::NAN], 1),
        ("root", vec![1.0], 0),
    ] {
        assert!(
            rank_vector_candidates(root, std::iter::empty(), &query, k, "engine", None).is_err()
        );
    }
    assert!(
        rank_vector_candidates("root", std::iter::empty(), &[1.0], 1, "engine", None)
            .unwrap()
            .is_empty()
    );
}
