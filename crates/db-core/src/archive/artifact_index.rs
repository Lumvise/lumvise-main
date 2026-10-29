//! Validates the physical attachment projection; revision policy supplies its selected associations.
use crate::{DbError, Result, SemanticRevisionAssociation};
use std::collections::{BTreeSet, HashMap};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ArtifactLocator {
    element_id: String,
    artifact_id: String,
    target_entry: String,
    row_group: usize,
}

fn locators(tables: &HashMap<String, Vec<u8>>) -> Result<Vec<ArtifactLocator>> {
    super::validation::read_batches(&tables["artifact_index.parquet"], None)?
        .into_iter()
        .map(|row| {
            let row_group = row[3].parse().map_err(|_| {
                DbError::invalid_value(&row[3], "a nonnegative artifact row-group index")
            })?;
            Ok(ArtifactLocator {
                element_id: row[0].clone(),
                artifact_id: row[1].clone(),
                target_entry: row[2].clone(),
                row_group,
            })
        })
        .collect()
}

pub(super) fn validate(
    tables: &HashMap<String, Vec<u8>>,
    group_counts: &HashMap<String, usize>,
) -> Result<()> {
    let rows = locators(tables)?;
    let group_count = group_counts["artifacts.parquet"];
    if rows.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(DbError::invalid_value(
            "artifact_index",
            "unique, canonically ordered locators",
        ));
    }
    let mut groups = HashMap::new();
    for locator in &rows {
        validate_locator(locator, group_count)?;
        if !groups.contains_key(&locator.row_group) {
            groups.insert(
                locator.row_group,
                super::validation::read_batches(
                    &tables["artifacts.parquet"],
                    Some(&BTreeSet::from([locator.row_group])),
                )?,
            );
        }
        if !groups[&locator.row_group]
            .iter()
            .any(|row| row[0] == locator.artifact_id && row[10] == "true")
        {
            return Err(DbError::invalid_value(
                &locator.artifact_id,
                "a selected artifact in its declared row group",
            ));
        }
    }
    Ok(())
}

fn validate_locator(locator: &ArtifactLocator, group_count: usize) -> Result<()> {
    if locator.element_id.is_empty()
        || locator.artifact_id.is_empty()
        || locator.target_entry != "artifacts.parquet"
        || locator.row_group >= group_count
    {
        return Err(DbError::invalid_value(
            format!("{locator:?}"),
            "a physical artifact row-group locator",
        ));
    }
    Ok(())
}

pub(super) fn validate_associations(
    tables: &HashMap<String, Vec<u8>>,
    associations: &[SemanticRevisionAssociation],
) -> Result<()> {
    let actual = locators(tables)?
        .into_iter()
        .map(|row| (row.element_id, row.artifact_id))
        .collect::<BTreeSet<_>>();
    let expected = associations
        .iter()
        .map(|row| (row.semantic_element_id.clone(), row.artifact_id.clone()))
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(DbError::invalid_value(
            "artifact_index",
            "the exact selected revision associations",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::{TABLE_SCHEMAS, encode_table};

    fn index_tables(group: &str) -> HashMap<String, Vec<u8>> {
        let columns = TABLE_SCHEMAS
            .iter()
            .find(|(name, _, _)| *name == "artifact_index.parquet")
            .unwrap()
            .2;
        let rows = vec![vec![
            Some("successor".into()),
            Some("note".into()),
            Some("artifacts.parquet".into()),
            Some(group.into()),
        ]];
        HashMap::from([(
            "artifact_index.parquet".into(),
            encode_table(columns, rows.len(), &rows).unwrap(),
        )])
    }

    #[test]
    fn attachment_projection_rejects_invalid_physical_locators_and_unselected_associations() {
        let counts = HashMap::from([("artifacts.parquet".into(), 1)]);
        assert!(validate(&index_tables("-1"), &counts).is_err());
        assert!(validate(&index_tables("1"), &counts).is_err());
        assert!(validate_associations(&index_tables("0"), &[]).is_err());
    }
}
