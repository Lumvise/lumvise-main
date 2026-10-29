//! Transitive exact dependencies and intersecting memberships are fetched without complete heads.
use super::*;

impl ReadPlan {
    pub fn expand(&mut self, dependencies: bool) -> SemanticRevisionResult<()> {
        for metadata in self.metadata.clone() {
            self.intersections(&metadata)?;
            if dependencies {
                self.dependencies(&metadata)?;
            }
        }
        Ok(())
    }

    fn intersections(&mut self, metadata: &SemanticRevisionMetadata) -> SemanticRevisionResult<()> {
        match metadata {
            SemanticRevisionMetadata::Record(header) => self.index(
                "member_record",
                &serde_json::to_string(&header.record)?,
                None,
            ),
            SemanticRevisionMetadata::Connection(connection) => {
                self.index("member_connection", &connection.connection_id, None)
            }
            SemanticRevisionMetadata::Membership(member) => {
                self.selection(&member.selection_id, false)
            }
            _ => {}
        }
        Ok(())
    }

    fn dependencies(&mut self, metadata: &SemanticRevisionMetadata) -> SemanticRevisionResult<()> {
        use SemanticRevisionMetadata::*;
        match metadata {
            Record(header) => {
                let key = StorageRowKey {
                    kind: "record".into(),
                    identity: metadata.identity()?,
                };
                if let Some(body) = self.rows[&key].body_json.as_ref() {
                    let body = super::super::content::decode(body)?.record;
                    self.records(body.intrinsic_dependencies())?;
                    if let SemanticRevisionBody::Artifact(artifact) = body {
                        self.records([SemanticRecordRef::element(&artifact.semantic_element_id)])?;
                    }
                }
                if header.record.kind == SemanticRecordKind::Artifact {
                    self.index("association_artifact", &header.record.record_id, None);
                }
            }
            Connection(connection) => self.records([
                SemanticRecordRef::element(&connection.relationship.source_element_id),
                SemanticRecordRef::element(&connection.relationship.target_element_id),
            ])?,
            Association(association) => self.records([
                SemanticRecordRef::artifact(&association.artifact_id),
                SemanticRecordRef::element(&association.semantic_element_id),
            ])?,
            Membership(member) => match &member.member {
                SemanticRevisionMember::Record(record) => self.records([record.clone()])?,
                SemanticRevisionMember::Connection(id) => self.key("connection", id),
            },
            _ => {}
        }
        Ok(())
    }

    pub fn owned_connections(
        &mut self,
        selection: &str,
        coverage: &SemanticRevisionStructureCoverage,
    ) -> SemanticRevisionResult<()> {
        let elements = self
            .metadata
            .iter()
            .filter_map(|row| match row {
                SemanticRevisionMetadata::Record(header) if in_scope(header, coverage) => {
                    Some(header.record.record_id.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for id in elements {
            self.index("connection_source", &id, Some(selection));
            self.index("containment_target", &id, Some(selection));
            self.index("association_element", &id, None);
        }
        Ok(())
    }
}

fn in_scope(
    header: &SemanticRevisionRecordHeader,
    coverage: &SemanticRevisionStructureCoverage,
) -> bool {
    let Some(scope) = &header.element_scope else {
        return false;
    };
    match coverage {
        SemanticRevisionStructureCoverage::None => false,
        SemanticRevisionStructureCoverage::Project => true,
        SemanticRevisionStructureCoverage::Partition(partition) => {
            partition.contains_path(&scope.path)
                && partition
                    .provider_instance_id
                    .as_ref()
                    .is_none_or(|id| scope.provider_instance_id.as_ref() == Some(id))
        }
    }
}
