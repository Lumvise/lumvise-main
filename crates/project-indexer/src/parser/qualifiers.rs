//! Preserves syntax evidence. Unknown receiver types must not become name-only edges.
use crate::ReferenceQualifier;
use tree_sitter::Node;

pub(super) fn reference_qualifier(node: Node<'_>, text: &str) -> ReferenceQualifier {
    let Some(parent) = node.parent() else {
        return ReferenceQualifier::Unqualified;
    };
    let (field, receiver) = match parent.kind() {
        "scoped_identifier" | "scoped_type_identifier" => ("path", false),
        "qualified_identifier" => ("scope", false),
        "field_expression" => ("value", true),
        "member_expression" | "attribute" => ("object", true),
        "selector_expression" => ("operand", true),
        "member_access_expression" => ("expression", true),
        _ => {
            return enclosing_type(node, text)
                .map(ReferenceQualifier::UnqualifiedInType)
                .unwrap_or_default();
        }
    };
    let Some(prefix) = parent.child_by_field_name(field) else {
        return ReferenceQualifier::Receiver(text[parent.byte_range()].into());
    };
    qualify(&text[prefix.byte_range()], receiver, node, text)
}

fn qualify(prefix: &str, receiver: bool, node: Node<'_>, text: &str) -> ReferenceQualifier {
    if matches!(prefix, "self" | "Self" | "this") {
        return enclosing_type(node, text)
            .map(ReferenceQualifier::SelfScope)
            .unwrap_or_else(|| ReferenceQualifier::Receiver(prefix.into()));
    }
    if receiver {
        return ReferenceQualifier::Receiver(prefix.into());
    }
    ReferenceQualifier::Scope(normalize_type_name(prefix).into())
}

pub(super) fn normalize_type_name(name: &str) -> &str {
    name.split('<')
        .next()
        .unwrap_or(name)
        .trim()
        .trim_end_matches("::")
}

pub(super) fn enclosing_type(mut node: Node<'_>, text: &str) -> Option<String> {
    while let Some(parent) = node.parent() {
        if let Some(field) = type_field(parent.kind()) {
            return parent
                .child_by_field_name(field)
                .map(|name| normalize_type_name(&text[name.byte_range()]).into());
        }
        node = parent;
    }
    None
}

pub(super) fn definition_type(mut node: Node<'_>, text: &str) -> Option<String> {
    // C/C++ queries capture the declarator, whose parent is its own function body.
    if node.kind() == "function_declarator"
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "function_definition")
    {
        node = node.parent()?;
    }
    while let Some(parent) = node.parent() {
        if function_boundary(parent.kind()) {
            return None;
        }
        if type_field(parent.kind()).is_some() {
            return enclosing_type(node, text);
        }
        node = parent;
    }
    None
}

fn type_field(kind: &str) -> Option<&'static str> {
    match kind {
        "impl_item" => Some("type"),
        "class_definition"
        | "class_declaration"
        | "abstract_class_declaration"
        | "class_expression"
        | "class"
        | "struct_declaration"
        | "interface_declaration"
        | "class_specifier"
        | "struct_specifier" => Some("name"),
        _ => None,
    }
}

fn function_boundary(kind: &str) -> bool {
    matches!(
        kind,
        "function_item"
            | "closure_expression"
            | "function_definition"
            | "lambda"
            | "function_declaration"
            | "function_expression"
            | "function"
            | "arrow_function"
            | "method_definition"
            | "generator_function"
            | "generator_function_declaration"
            | "lambda_expression"
            | "method_declaration"
            | "constructor_declaration"
            | "local_function_statement"
            | "accessor_declaration"
            | "anonymous_method_expression"
    )
}
