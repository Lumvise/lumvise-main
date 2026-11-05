//! Canonical physical membership copies represent one set member; other duplicate metadata stays invalid.
use super::*;

pub(in super::super) fn validate_membership_row_copies(
    graph: &GrafeoDB,
    root: &str,
    key: &str,
    copies: &[NodeId],
) -> Result<()> {
    let unique = copies.iter().collect::<std::collections::BTreeSet<_>>();
    if unique.len() != copies.len() {
        return Err(DbError::invalid_value(
            key,
            "distinct physical membership rows",
        ));
    }
    for id in copies {
        validate_canonical_membership_copy(graph, root, key, *id)?;
    }
    Ok(())
}

fn validate_canonical_membership_copy(
    graph: &GrafeoDB,
    root: &str,
    key: &str,
    id: NodeId,
) -> Result<()> {
    let row = revision_row_from_node(graph, id)?;
    require_membership_copy_identity(root, key, &row)?;
    validate_membership_copy_properties(graph, key, id, &serde_json::to_string(&row)?)?;
    validate_membership_copy_lookups(graph, root, key, id, &row)
}

fn require_membership_copy_identity(root: &str, key: &str, row: &RevisionRow) -> Result<()> {
    let RevisionRow::Membership(member) = row else {
        return Err(DbError::invalid_value(
            key,
            "one stored row per non-membership revision key",
        ));
    };
    let member_id = match &member.member {
        RevisionMember::Record(record) => &record.record_id,
        RevisionMember::Connection(id) => id,
    };
    if member.selection_id.trim().is_empty() || member_id.trim().is_empty() || row.key(root)? != key
    {
        return Err(DbError::invalid_value(
            key,
            "a nonempty membership matching its canonical key",
        ));
    }
    Ok(())
}

fn validate_membership_copy_properties(
    graph: &GrafeoDB,
    key: &str,
    id: NodeId,
    canonical: &str,
) -> Result<()> {
    let node = graph
        .get_node(id)
        .db()?
        .filter(|node| node.has_label(ROW_LABEL))
        .ok_or_else(|| {
            DbError::invalid_value(id.to_string(), "a visible revision membership row")
        })?;
    if node.get_property(ROW_KEY).and_then(Value::as_str) != Some(key)
        || node.get_property(ROW_PAYLOAD).and_then(Value::as_str) != Some(canonical)
    {
        return Err(DbError::invalid_value(
            key,
            "byte-identical canonical membership copies",
        ));
    }
    Ok(())
}

fn validate_membership_copy_lookups(
    graph: &GrafeoDB,
    root: &str,
    key: &str,
    id: NodeId,
    row: &RevisionRow,
) -> Result<()> {
    let expected = membership_copy_lookups(root, row)?;
    let snapshot = graph.committed_snapshot()?;
    for (property, expected) in expected {
        // GRAFEO06-REVIEW(27): committed property read; 0.5 read the live store column.
        let actual = snapshot.node_property(id, property, snapshot.view()).db()?;
        if actual.as_ref().and_then(Value::as_str) != expected.as_deref() {
            return Err(DbError::invalid_value(
                key,
                format!("canonical membership lookup property {property}"),
            ));
        }
    }
    Ok(())
}

fn membership_copy_lookups(
    root: &str,
    row: &RevisionRow,
) -> Result<Vec<(&'static str, Option<String>)>> {
    let RevisionRow::Membership(member) = row else {
        unreachable!("validated membership")
    };
    let (record, connection) = match &member.member {
        RevisionMember::Record(record) => (Some(serde_json::to_string(record)?), None),
        RevisionMember::Connection(connection) => (None, Some(connection.clone())),
    };
    let selection = serde_json::to_string(&(root, &member.selection_id))?;
    Ok(vec![
        (ROW_PROJECT, Some(root.to_owned())),
        (SELECTION_KEY, Some(selection)),
        (MEMBERSHIP_IDENTITY, record),
        ("revision_membership_connection", connection),
    ])
}
