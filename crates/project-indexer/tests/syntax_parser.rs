use lumvise_project_indexer::{ParseStatus, ProjectFileParser, TreeSitterProjectParser};

#[test]
fn all_grammars_extract_definitions_and_calls_from_one_tree() {
    let fixtures = [
        ("sample.rs", "fn caller() { target(); }", "rust"),
        ("sample.py", "def caller():\n    target()\n", "python"),
        ("sample.js", "function caller() { target(); }", "javascript"),
        (
            "sample.ts",
            "function caller(): void { target(); }",
            "typescript",
        ),
        (
            "sample.tsx",
            "function caller() { target(); return <div/>; }",
            "tsx",
        ),
        (
            "sample.go",
            "package main\nfunc caller() { target() }",
            "go",
        ),
        ("sample.c", "void caller() { target(); }", "c"),
        ("sample.cpp", "void caller() { target(); }", "cpp"),
        (
            "sample.cs",
            "class Sample { void caller() { target(); } }",
            "csharp",
        ),
    ];
    let mut parser = TreeSitterProjectParser::default();
    for (path, text, language) in fixtures {
        let parsed = parser.parse(path, text.as_bytes()).unwrap();
        assert_eq!(
            parsed.status,
            ParseStatus::Parsed {
                language: language.into(),
                has_syntax_errors: false
            },
            "{path}"
        );
        assert!(
            parsed.definitions.iter().any(|item| item.name == "caller"),
            "{path}: {:?}",
            parsed.definitions
        );
        assert!(
            parsed
                .references
                .iter()
                .any(|item| item.name == "target" && item.kind == "calls"),
            "{path}: {:?}",
            parsed.references
        );
        assert_eq!(parsed.source.as_deref(), Some(text));
    }
    assert_eq!(parser.metrics().trees_parsed, 9);
    assert_eq!(parser.metrics().languages_initialized, 9);
}

#[test]
fn parser_reuse_keeps_files_independent_and_ignores_comments_and_strings() {
    let mut parser = TreeSitterProjectParser::default();
    parser
        .parse("first.rs", b"fn previous() { stale(); }")
        .unwrap();
    let parsed = parser
        .parse(
            "second.RS",
            b"fn current() { actual(); /* fake(); */ let text = \"fake();\"; }",
        )
        .unwrap();
    assert_eq!(
        parsed
            .definitions
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        vec!["current"]
    );
    assert_eq!(
        parsed
            .references
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        vec!["actual"]
    );
    assert_eq!(parser.metrics().languages_initialized, 1);
    assert_eq!(parser.metrics().trees_parsed, 2);
}

#[test]
fn rust_constants_statics_and_bodyless_functions_keep_complete_declaration_spans() {
    let text = "pub const LIMIT: usize = 8;\nstatic LABEL: &str = \"value\";\ntrait Contract { fn required(&self); }\nunsafe extern \"C\" { fn external(); }";
    let mut parser = TreeSitterProjectParser::default();
    let parsed = parser.parse("sample.rs", text.as_bytes()).unwrap();
    for (name, kind, body) in [
        ("LIMIT", "constant", "pub const LIMIT: usize = 8;"),
        ("LABEL", "static", "static LABEL: &str = \"value\";"),
        ("required", "function", "fn required(&self);"),
        ("external", "function", "fn external();"),
    ] {
        let definition = parsed
            .definitions
            .iter()
            .find(|item| item.name == name)
            .unwrap_or_else(|| panic!("missing {name}: {:?}", parsed.definitions));
        assert_eq!(definition.kind, kind, "{name}");
        assert_eq!(&text[definition.span.start..definition.span.end], body);
    }
    assert_eq!(parser.metrics().trees_parsed, 1);
}

#[test]
fn rust_module_functions_are_distinct_from_implementation_and_trait_methods() {
    let text = "mod nested { fn standalone() {} } struct Sample; impl Sample { fn associated() {} } trait Contract { fn default_method() {} }";
    let parsed = TreeSitterProjectParser::default()
        .parse("sample.rs", text.as_bytes())
        .unwrap();
    for (name, kind) in [
        ("standalone", "function"),
        ("associated", "method"),
        ("default_method", "method"),
    ] {
        let definitions: Vec<_> = parsed
            .definitions
            .iter()
            .filter(|definition| definition.name == name)
            .collect();
        assert_eq!(definitions.len(), 1, "{name}");
        assert_eq!(definitions[0].kind, kind, "{name}");
    }
}

#[test]
fn rust_methods_keep_exact_bodies_and_implementation_target() {
    let text =
        "struct Sample;\nimpl Sample {\n fn first() { external::target(); }\n fn second() {}\n}\n";
    let parsed = TreeSitterProjectParser::default()
        .parse("sample.rs", text.as_bytes())
        .unwrap();
    let first = parsed
        .definitions
        .iter()
        .filter(|item| item.name == "first")
        .collect::<Vec<_>>();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].kind, "method");
    assert_eq!(first[0].implementation_type.as_deref(), Some("Sample"));
    assert_eq!(
        &text[first[0].span.start..first[0].span.end],
        "fn first() { external::target(); }"
    );
    assert_eq!((first[0].start_line, first[0].end_line), (3, 3));
    assert!(parsed.references.iter().any(|item| item.name == "target"));
}

#[test]
fn unicode_and_same_line_references_preserve_distinct_byte_spans() {
    let text = "fn café() { target(); target(); }";
    let parsed = TreeSitterProjectParser::default()
        .parse("sample.rs", text.as_bytes())
        .unwrap();
    assert!(parsed.definitions.iter().any(|item| item.name == "café"));
    assert_eq!(parsed.references.len(), 2);
    assert_ne!(parsed.references[0].span, parsed.references[1].span);
    for reference in parsed.references {
        assert_eq!(&text[reference.span.start..reference.span.end], "target");
    }
}

#[test]
fn unsupported_binary_and_recovered_syntax_are_explicit() {
    let mut parser = TreeSitterProjectParser::default();
    let unsupported = parser.parse("junk.notes", b"").unwrap();
    assert_eq!(unsupported.status, ParseStatus::Unsupported);
    let text = parser
        .parse("junk.notes", b"# Kept for content projection")
        .unwrap();
    assert_eq!(text.status, ParseStatus::PlainText);
    assert_eq!(text.definitions.len(), 1);
    assert_eq!(text.definitions[0].kind, "block");
    assert_eq!(
        text.source.as_deref(),
        Some("# Kept for content projection")
    );
    let heading = parser
        .parse("notes.md", b"# Kept for content projection")
        .unwrap();
    assert_eq!(heading.definitions.len(), 1);
    assert_eq!(heading.definitions[0].name, "Kept for content projection");
    assert_eq!(heading.definitions[0].kind, "heading");
    assert_eq!(
        parser.parse("photo.png", &[0xff]).unwrap().status,
        ParseStatus::Binary
    );
    assert_eq!(
        parser.parse("binary.rs", &[0xff]).unwrap().status,
        ParseStatus::Binary
    );
    assert_eq!(
        parser.parse("binary.rs", b"\0").unwrap().status,
        ParseStatus::Binary
    );
    let parsed = parser
        .parse("partial.rs", b"fn valid() {}\nfn broken( {")
        .unwrap();
    assert!(matches!(
        parsed.status,
        ParseStatus::Parsed {
            has_syntax_errors: true,
            ..
        }
    ));
    assert!(parsed.definitions.iter().any(|item| item.name == "valid"));
    assert_eq!(parser.metrics().trees_parsed, 2);
}

#[test]
fn text_fallback_indexes_blank_line_blocks_with_settings() {
    use lumvise_project_indexer::BlockSettings;

    let text = "alpha one\nalpha two\n\nbeta\n\ngamma\n\n\ndelta\n";
    let parsed = TreeSitterProjectParser::default()
        .parse("data.table", text.as_bytes())
        .unwrap();
    assert_eq!(parsed.status, ParseStatus::PlainText);
    let names: Vec<&str> = parsed.definitions.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["alpha one", "beta", "gamma", "delta"]);
    assert_eq!(parsed.definitions[0].start_line, 1);
    assert_eq!(parsed.definitions[0].end_line, 2);
    assert_eq!(parsed.definitions[1].start_line, 4);

    let capped = TreeSitterProjectParser::default()
        .with_block_settings(BlockSettings {
            max_blocks_per_file: 2,
            max_file_lines: 50,
        })
        .parse("data.table", text.as_bytes())
        .unwrap();
    assert_eq!(capped.status, ParseStatus::PlainText);
    assert_eq!(capped.definitions.len(), 2);

    let oversized = TreeSitterProjectParser::default()
        .with_block_settings(BlockSettings {
            max_blocks_per_file: 50,
            max_file_lines: 3,
        })
        .parse("huge.log", text.as_bytes())
        .unwrap();
    assert_eq!(oversized.status, ParseStatus::Unsupported);
    assert!(oversized.definitions.is_empty());
    assert_eq!(
        oversized.source.as_deref(),
        Some("alpha one\nalpha two\n\nbeta\n\ngamma\n\n\ndelta\n")
    );
}
