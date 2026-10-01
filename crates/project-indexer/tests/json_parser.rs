use lumvise_project_indexer::{ParseStatus, ProjectFileParser, TreeSitterProjectParser};

#[test]
fn minified_json_entries_keep_distinct_spans_and_nested_values_stay_with_the_owner() {
    let text = r#"{"enabled":true,"nested":{"child":1},"items":[1,2],"message":"}, not a new key","count":12,"nothing":null}"#;
    let mut parser = TreeSitterProjectParser::default();
    let parsed = parser.parse("settings.json", text.as_bytes()).unwrap();
    assert_eq!(
        parsed.status,
        ParseStatus::Parsed {
            language: "json".into(),
            has_syntax_errors: false
        }
    );
    let entries: Vec<_> = parsed
        .definitions
        .iter()
        .map(|entry| {
            (
                entry.name.as_str(),
                entry.kind.as_str(),
                &text[entry.span.start..entry.span.end],
            )
        })
        .collect();
    assert_eq!(
        entries,
        [
            ("enabled", "property", r#""enabled":true"#),
            ("nested", "object", r#""nested":{"child":1}"#),
            ("items", "object", r#""items":[1,2]"#),
            ("message", "property", r#""message":"}, not a new key""#),
            ("count", "property", r#""count":12"#),
            ("nothing", "property", r#""nothing":null"#),
        ]
    );
    assert!(parsed.references.is_empty());
    assert_eq!(parser.metrics().trees_parsed, 1);
}

#[test]
fn json_names_decode_escapes_and_multiline_ranges_cover_complete_values() {
    let text = "{\n  \"caf\\u00e9\": {\n    \"nested\": 3\n  },\n  \"\\\"quoted\\\"\": false,\n  \"\": 0\n}";
    let parsed = TreeSitterProjectParser::default()
        .parse("settings.JSON", text.as_bytes())
        .unwrap();
    assert_eq!(parsed.definitions.len(), 2);
    assert_eq!(parsed.definitions[0].name, "café");
    assert_eq!(
        (
            parsed.definitions[0].start_line,
            parsed.definitions[0].end_line
        ),
        (2, 4)
    );
    assert_eq!(parsed.definitions[1].name, "\"quoted\"");
}

#[test]
fn json_root_arrays_do_not_invent_properties_and_invalid_syntax_is_explicit() {
    let mut parser = TreeSitterProjectParser::default();
    let array = parser.parse("array.json", br#"[{"nested":1}]"#).unwrap();
    assert!(array.definitions.is_empty());
    let invalid = parser.parse("broken.json", br#"{"broken": }"#).unwrap();
    assert_eq!(
        invalid.status,
        ParseStatus::Parsed {
            language: "json".into(),
            has_syntax_errors: true
        }
    );
    assert_eq!(parser.metrics().languages_initialized, 1);
    assert_eq!(parser.metrics().trees_parsed, 2);
}
