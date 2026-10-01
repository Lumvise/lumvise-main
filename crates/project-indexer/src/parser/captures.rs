use crate::{IndexedDefinition, IndexedReference, ParsedFile, SourceSpan};
use std::collections::BTreeMap;
use tree_sitter::{Node, Query, QueryCursor, QueryMatch, StreamingIterator, Tree};

pub(super) fn extract(
    query: &Query,
    cursor: &mut QueryCursor,
    tree: &Tree,
    text: &str,
) -> ParsedFile {
    let mut definitions = BTreeMap::new();
    let mut references = BTreeMap::new();
    let mut matches = cursor.matches(query, tree.root_node(), text.as_bytes());
    while let Some(found) = matches.next() {
        collect_match(query, found, text, &mut definitions, &mut references);
    }
    if tree.root_node().kind() == "compilation_unit" {
        collect_csharp_local_functions(tree.root_node(), text, &mut definitions);
    }
    ParsedFile {
        definitions: definitions.into_values().collect(),
        references: references.into_values().collect(),
        ..ParsedFile::default()
    }
}

type DefinitionCaptures = BTreeMap<(SourceSpan, String), IndexedDefinition>;
type ReferenceCaptures = BTreeMap<(SourceSpan, String, String), IndexedReference>;

fn collect_match(
    query: &Query,
    found: &QueryMatch<'_, '_>,
    text: &str,
    definitions: &mut DefinitionCaptures,
    references: &mut ReferenceCaptures,
) {
    for capture in found.captures {
        let tag = query.capture_names()[capture.index as usize];
        if let Some(kind) = tag.strip_prefix("name.definition.") {
            let body = declaration_node(query, found, &tag[5..], capture.node);
            insert_definition(definitions, definition(kind, capture.node, body, text));
        }
        if let Some(kind) = tag.strip_prefix("name.reference.") {
            let reference = reference(kind, capture.node, text);
            references.insert(
                (
                    reference.span,
                    reference.name.clone(),
                    reference.kind.clone(),
                ),
                reference,
            );
        }
    }
}

fn declaration_node<'tree>(
    query: &Query,
    found: &QueryMatch<'_, 'tree>,
    tag: &str,
    name: Node<'tree>,
) -> Node<'tree> {
    found
        .captures
        .iter()
        .filter(|capture| query.capture_names()[capture.index as usize] == tag)
        .map(|capture| capture.node)
        .filter(|node| node.start_byte() <= name.start_byte() && node.end_byte() >= name.end_byte())
        .min_by_key(|node| node.byte_range().len())
        .unwrap_or(name)
}

fn definition(kind: &str, name: Node<'_>, body: Node<'_>, text: &str) -> IndexedDefinition {
    let spelling = &text[name.byte_range()];
    IndexedDefinition {
        kind: kind.rsplit('.').next().unwrap_or(kind).into(),
        name: if kind.starts_with("json.") {
            serde_json::from_str(spelling).unwrap_or_else(|_| spelling.into())
        } else {
            spelling.into()
        },
        start_line: body.start_position().row + 1,
        end_line: body.end_position().row + 1,
        span: span(body),
        implementation_type: super::qualifiers::definition_type(body, text),
    }
}

fn insert_definition(definitions: &mut DefinitionCaptures, definition: IndexedDefinition) {
    // The semantic element contract requires a non-empty name. Empty JSON keys
    // remain represented by their file, as in the existing document extractor.
    if definition.name.is_empty() {
        return;
    }
    let key = (definition.span, definition.name.clone());
    if definitions
        .get(&key)
        .is_some_and(|old| kind_priority(&old.kind) > kind_priority(&definition.kind))
    {
        return;
    }
    definitions.insert(key, definition);
}

fn kind_priority(kind: &str) -> u8 {
    // Overlapping generic/specific queries describe one declaration, not two IDs.
    match kind {
        "constructor" => 3,
        "method" => 2,
        "function" => 1,
        _ => 0,
    }
}

fn reference(kind: &str, node: Node<'_>, text: &str) -> IndexedReference {
    IndexedReference {
        name: text[node.byte_range()].into(),
        line: node.start_position().row + 1,
        kind: if kind.starts_with("callable") {
            "calls"
        } else {
            "uses"
        }
        .into(),
        span: span(node),
        qualifier: super::qualifiers::reference_qualifier(node, text),
    }
}

fn span(node: Node<'_>) -> SourceSpan {
    SourceSpan {
        start: node.start_byte(),
        end: node.end_byte(),
    }
}

fn collect_csharp_local_functions(
    node: Node<'_>,
    text: &str,
    definitions: &mut DefinitionCaptures,
) {
    if node.kind() == "local_function_statement"
        && let Some(name) = node.child_by_field_name("name")
    {
        insert_definition(definitions, definition("function", name, node, text));
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_csharp_local_functions(child, text, definitions);
    }
}
