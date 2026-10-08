use super::*;

pub(crate) struct PzSnapshotData {
    pub(crate) snapshot: SemanticProjectSnapshot,
    pub(crate) artifact_blobs: Vec<ArtifactBlob>,
    pub(crate) artifact_text_vectors: Vec<StoredArtifactTextVector>,
    pub(crate) element_name_vectors: Vec<StoredSemanticElementNameVector>,
}
impl<'db> SemanticStorage<'db> {
    /// Extracts one complete published project graph under the graph writer gate.
    pub fn project_snapshot(
        &self,
        scope: &ProjectSnapshotScope,
        artifact_namespace: Option<&str>,
    ) -> Result<SemanticProjectSnapshot> {
        match scope {
            ProjectSnapshotScope::ProjectRoot(project_root) => {
                require_non_empty(project_root, "non-empty project root")?
            }
            ProjectSnapshotScope::SemanticElement(element_id) => {
                require_non_empty(element_id, "non-empty semantic element id")?
            }
        }
        self.graph.stable_read(|graph| {
            let commit_version = latest_published_commit_version(&self.conn.read_conn())?;
            let published_at = published_commit_timestamp(&self.conn.read_conn(), commit_version)?;
            let mut snapshot = project_snapshot(
                graph,
                scope,
                artifact_namespace,
                commit_version,
                published_at,
            )?;
            self.hydrate_artifact_contents(&mut snapshot.artifacts)?;
            Ok(snapshot)
        })
    }
    /// Reads every raw SQL blob one already-filtered snapshot's artifacts own:
    /// their referenced content payloads plus attachments such as canvas images.
    pub(crate) fn artifact_blobs_for_snapshot(
        &self,
        artifacts: &[SemanticArtifact],
    ) -> Result<Vec<ArtifactBlob>> {
        let repository = ArtifactBlobRepository::with_clock(self.conn, self.clock.clone());
        let refs = artifacts
            .iter()
            .filter_map(|artifact| artifact.content_ref.as_deref())
            .collect::<Vec<_>>();
        let mut blobs = repository.blobs_for_content_refs(&refs)?;
        let ids = artifacts
            .iter()
            .map(|artifact| artifact.artifact_id.as_str())
            .collect::<Vec<_>>();
        for blob in repository.blobs_for_artifacts(&ids)? {
            blobs.entry(blob.content_ref.clone()).or_insert(blob);
        }
        let mut selected = blobs.into_values().collect::<Vec<_>>();
        selected.sort_by(|left, right| left.content_ref.cmp(&right.content_ref));
        Ok(selected)
    }
    /// Captures every PZ-owned row source under one stable graph lease and
    /// published revision.
    pub(crate) fn pz_snapshot_data(&self, project_root: &str) -> Result<PzSnapshotData> {
        require_non_empty(project_root, "non-empty project root")?;
        self.graph.stable_read(|graph| {
            let commit_version = latest_published_commit_version(&self.conn.read_conn())?;
            let published_at = published_commit_timestamp(&self.conn.read_conn(), commit_version)?;
            let snapshot = project_snapshot(
                graph,
                &ProjectSnapshotScope::ProjectRoot(project_root.to_owned()),
                None,
                commit_version,
                published_at,
            )?;
            let active_element_ids = snapshot
                .elements
                .iter()
                .filter(|element| element.lifecycle != "inactive")
                .map(|element| element.semantic_element_id.clone())
                .collect::<HashSet<_>>();
            let direct_artifact_ids = snapshot
                .artifacts
                .iter()
                .filter(|artifact| active_element_ids.contains(&artifact.semantic_element_id))
                .map(|artifact| {
                    (
                        artifact.artifact_id.clone(),
                        artifact.semantic_element_id.clone(),
                    )
                })
                .collect::<HashSet<_>>();
            let mut artifact_vectors = direct_artifact_ids
                .iter()
                .flat_map(|(artifact_id, semantic_element_id)| {
                    nodes_by_label_and_property(
                        graph,
                        "SemanticArtifactVector",
                        "artifact_id",
                        artifact_id,
                    )
                    .into_iter()
                    .filter_map(|node| artifact_vector_from_node(&node))
                    .filter(|vector| {
                        vector.artifact_id == *artifact_id
                            && vector.semantic_element_id == *semantic_element_id
                    })
                })
                .collect::<Vec<_>>();
            artifact_vectors.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
            let mut element_vectors = nodes_by_label_and_property(
                graph,
                "SemanticElementNameVector",
                "project_root",
                project_root,
            )
            .into_iter()
            .filter_map(|node| element_name_vector_from_node(&node))
            .filter(|vector| active_element_ids.contains(&vector.semantic_element_id))
            .collect::<Vec<_>>();
            element_vectors
                .sort_by(|left, right| left.semantic_element_id.cmp(&right.semantic_element_id));
            let blobs = self.artifact_blobs_for_snapshot(&snapshot.artifacts)?;
            Ok(PzSnapshotData {
                snapshot,
                artifact_blobs: blobs,
                artifact_text_vectors: artifact_vectors,
                element_name_vectors: element_vectors,
            })
        })
    }
    /// Extracts one target subtree and its touching graph records under one stable read.
    pub fn selective_subgraph(
        &self,
        project_root: &str,
        root_element_id: &str,
        artifact_namespace: Option<&str>,
    ) -> Result<Option<crate::SemanticSelectiveSubgraph>> {
        require_non_empty(project_root, "non-empty project root")?;
        require_non_empty(root_element_id, "non-empty root element id")?;
        self.graph.stable_read(|graph| {
            let commit_version = latest_published_commit_version(&self.conn.read_conn())?;
            let published_at = published_commit_timestamp(&self.conn.read_conn(), commit_version)?;
            let root_ids = HashSet::from([root_element_id.to_owned()]);
            let mut roots = elements_by_ids_selective(graph, project_root, &root_ids, true);
            let Some(root) = roots.pop() else {
                return Ok(None);
            };
            if root.lifecycle != "active" {
                return Ok(None);
            }
            let mut elements = collect_semantic_subtree(graph, root, usize::MAX);
            elements
                .sort_by(|left, right| left.semantic_element_id.cmp(&right.semantic_element_id));
            let element_ids = elements
                .iter()
                .map(|element| element.semantic_element_id.clone())
                .collect::<HashSet<_>>();
            let relationships = semantic_relationships_touching_elements(graph, &element_ids);
            let external_ids = relationships
                .iter()
                .flat_map(|relationship| {
                    [
                        relationship.source_element_id.as_str(),
                        relationship.target_element_id.as_str(),
                    ]
                })
                .filter(|id| !element_ids.contains(*id))
                .map(str::to_owned)
                .collect::<HashSet<_>>();
            let mut external_elements =
                elements_by_ids_selective(graph, project_root, &external_ids, true);
            external_elements
                .sort_by(|left, right| left.semantic_element_id.cmp(&right.semantic_element_id));
            let mut artifacts = semantic_artifacts_for_elements(graph, &element_ids);
            if let Some(namespace) = artifact_namespace {
                artifacts.retain(|artifact| artifact.metadata[namespace].is_object());
            }
            artifacts.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
            self.hydrate_artifact_contents(&mut artifacts)?;
            Ok(Some(crate::SemanticSelectiveSubgraph {
                commit_version,
                published_at,
                project_root: project_root.to_owned(),
                root_element_id: root_element_id.to_owned(),
                elements,
                relationships,
                artifacts,
                external_elements,
            }))
        })
    }
    /// Projects one renderer graph view. The graph read gate is held only for
    /// graph-derived work: the SQL projection-cache write happens after the gate
    /// is released, so a blocked SQL writer can never stall graph writers that
    /// wait on this reader.
    pub fn project_renderer_graph(
        &self,
        request: &crate::SemanticGraphProjectionRequest,
    ) -> Result<crate::SemanticGraphProjection> {
        crate::local::sql::validation::require_non_empty(
            &request.project_root,
            "non-empty project root",
        )?;
        let started = Instant::now();
        let (projection, pending_persist) = self.graph.stable_read(|graph| {
            let (commit_version, published_at) = self.latest_graph_publication()?;
            let projection_started = Instant::now();
            let (mut projection, pending_persist) =
                self.renderer_projection(graph, request, commit_version, published_at)?;
            metrics::histogram!("lumvise_db_renderer_graph_stage_seconds", "stage" => "projection")
                .record(projection_started.elapsed().as_secs_f64());
            let hydration_started = Instant::now();
            self.hydrate_projection_artifacts(&mut projection)?;
            metrics::histogram!("lumvise_db_renderer_graph_stage_seconds", "stage" => "artifact_hydration")
                .record(hydration_started.elapsed().as_secs_f64());
            metrics::histogram!("lumvise_db_renderer_graph_stage_seconds", "stage" => "total")
                .record(started.elapsed().as_secs_f64());
            Ok((projection, pending_persist))
        })?;
        if let Some(canonical) = pending_persist {
            let conn = self.conn.write_conn();
            SemanticGraphProjectionRepository::new(&conn).put(&canonical)?;
        }
        metrics::gauge!("lumvise_db_renderer_graph_nodes").set(projection.nodes.len() as f64);
        metrics::gauge!("lumvise_db_renderer_graph_edges").set(projection.edges.len() as f64);
        metrics::gauge!("lumvise_db_renderer_graph_artifacts").set(
            projection
                .nodes
                .iter()
                .map(|node| node.artifacts.len())
                .sum::<usize>() as f64,
        );
        Ok(projection)
    }

    fn renderer_projection(
        &self,
        graph: &grafeo::GrafeoDB,
        request: &crate::SemanticGraphProjectionRequest,
        commit_version: i64,
        published_at: String,
    ) -> Result<RendererProjectionOutcome> {
        if request.granularity == crate::SemanticGraphGranularity::File {
            return self.file_projection(graph, request, commit_version, published_at);
        }
        Ok((
            project_renderer_graph(graph, request, commit_version, published_at)?,
            None,
        ))
    }

    /// Projection plus an optional still-unpersisted canonical projection. The
    /// canonical copy is persisted by the caller after the graph read gate is
    /// released, keeping the SQL write mutex out of the graph gate.
    fn file_projection(
        &self,
        graph: &grafeo::GrafeoDB,
        request: &crate::SemanticGraphProjectionRequest,
        commit_version: i64,
        published_at: String,
    ) -> Result<RendererProjectionOutcome> {
        if let Some(projection) = self.graph.cached_projection(commit_version, request) {
            return Ok((projection, None));
        }
        let canonical_request = canonical_file_projection_request(&request.project_root);
        let (canonical, pending_persist) = self.canonical_file_projection(
            graph,
            &canonical_request,
            commit_version,
            published_at,
        )?;
        if request == &canonical_request {
            return Ok((canonical, pending_persist));
        }
        let projection = slice_file_projection(&canonical, request);
        self.graph
            .cache_projection(commit_version, request.clone(), projection.clone());
        Ok((projection, pending_persist))
    }

    fn canonical_file_projection(
        &self,
        graph: &grafeo::GrafeoDB,
        request: &crate::SemanticGraphProjectionRequest,
        commit_version: i64,
        published_at: String,
    ) -> Result<RendererProjectionOutcome> {
        if let Some(projection) = self.graph.cached_projection(commit_version, request) {
            return Ok((projection, None));
        }
        let stored = {
            let conn = self.conn.read_conn();
            SemanticGraphProjectionRepository::new(&conn)
                .get(&request.project_root, commit_version)?
        };
        match stored {
            Some(projection) => {
                self.graph
                    .cache_projection(commit_version, request.clone(), projection.clone());
                Ok((projection, None))
            }
            None => {
                // Computed purely from the gated graph snapshot; the caller
                // persists this exact unhydrated copy outside the gate.
                let projection =
                    project_renderer_graph(graph, request, commit_version, published_at)?;
                self.graph
                    .cache_projection(commit_version, request.clone(), projection.clone());
                Ok((projection.clone(), Some(projection)))
            }
        }
    }

    pub(crate) fn latest_graph_publication(&self) -> Result<(i64, String)> {
        let conn = self.conn.read_conn();
        let commit_version = latest_published_commit_version(&conn)?;
        let published_at = published_commit_timestamp(&conn, commit_version)?;
        Ok((commit_version, published_at))
    }

    fn hydrate_projection_artifacts(
        &self,
        projection: &mut crate::SemanticGraphProjection,
    ) -> Result<()> {
        let content_refs = projection
            .nodes
            .iter()
            .flat_map(|node| node.artifacts.iter())
            .filter(|artifact| artifact.text.is_none())
            .filter_map(|artifact| artifact.content_ref.as_deref())
            .collect::<Vec<_>>();
        let mut blobs = ArtifactBlobRepository::with_clock(self.conn, self.clock.clone())
            .blobs_for_content_refs(&content_refs)?;
        for artifact in projection
            .nodes
            .iter_mut()
            .flat_map(|node| node.artifacts.iter_mut())
        {
            if let Some(blob) = artifact
                .content_ref
                .as_deref()
                .and_then(|content_ref| blobs.remove(content_ref))
            {
                artifact.text = String::from_utf8(blob.content).ok();
            }
        }
        Ok(())
    }

    fn hydrate_artifact_contents(&self, artifacts: &mut [SemanticArtifact]) -> Result<()> {
        let content_refs = artifacts
            .iter()
            .filter(|artifact| artifact.content.is_none())
            .filter_map(|artifact| artifact.content_ref.as_deref())
            .collect::<Vec<_>>();
        let mut blobs = ArtifactBlobRepository::with_clock(self.conn, self.clock.clone())
            .blobs_for_content_refs(&content_refs)?;
        for artifact in artifacts {
            let Some(content_ref) = artifact.content_ref.as_deref() else {
                continue;
            };
            if let Some(blob) = blobs.remove(content_ref) {
                artifact.content = String::from_utf8(blob.content).ok();
                continue;
            }
            // The blob row is gone (e.g. a crash between the published graph
            // commit and the SQL replacement delete). For content within the
            // searchable limit the node still holds the full text, so the
            // artifact stays readable instead of poisoning every listing.
            if artifact
                .content_size_bytes
                .is_some_and(|size| size <= SEARCHABLE_ARTIFACT_TEXT_LIMIT_BYTES)
            {
                artifact.content = artifact.searchable_text.clone();
            }
        }
        Ok(())
    }
    /// Reads a semantic element by id.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_db_core::{LocalPersistence, SemanticOperation, SemanticPersistence, SemanticResult};
    /// use lumvise_resource_routing::InvocationControl;
    ///
    /// let persistence = LocalPersistence::in_memory().unwrap();
    /// let control = InvocationControl::sixty_seconds();
    /// let result = SemanticPersistence::execute(
    ///     &persistence,
    ///     SemanticOperation::Element { semantic_element_id: "missing".into() },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(result, SemanticResult::Element(None)));
    /// ```
    pub fn element(&self, semantic_element_id: &str) -> Result<Option<SemanticElement>> {
        require_non_empty(semantic_element_id, "non-empty semantic element id")?;
        Ok(self
            .graph
            .read(|graph| semantic_element_by_id(graph, semantic_element_id)))
    }

    /// Reads one semantic element and all descendants using indexed parent lookups.
    ///
    /// # Example
    ///
    /// ```
    /// let db = lumvise_db_core::DbCore::in_memory().unwrap();
    /// assert!(db.storage_manager().semantic_storage().elements_in_subtree("missing").unwrap().is_empty());
    /// ```
    #[cfg(test)]
    pub fn elements_in_subtree(&self, semantic_element_id: &str) -> Result<Vec<SemanticElement>> {
        self.elements_in_subtree_depth(semantic_element_id, usize::MAX)
    }

    /// Reads one semantic element and descendants no deeper than `maximum_depth`.
    #[cfg(test)]
    pub fn elements_in_subtree_depth(
        &self,
        semantic_element_id: &str,
        maximum_depth: usize,
    ) -> Result<Vec<SemanticElement>> {
        require_non_empty(semantic_element_id, "non-empty semantic element id")?;
        Ok(self.graph.read(|graph| {
            let Some(root) = semantic_element_by_id(graph, semantic_element_id) else {
                return Vec::new();
            };
            collect_semantic_subtree(graph, root, maximum_depth)
        }))
    }

    /// Returns a deterministic bounded lexical candidate set for Semantic search.
    pub fn search_element_candidates(
        &self,
        project_root: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SemanticElement>> {
        require_non_empty(query, "non-empty semantic element search query")?;
        if let Some(root) = project_root {
            require_non_empty(root, "non-empty project root")?;
        }
        self.graph
            .search_element_candidates(project_root, query, limit)
    }

    pub fn artifacts_for_element_with_inheritance(
        &self,
        semantic_element_id: &str,
    ) -> Result<Vec<SemanticArtifact>> {
        require_non_empty(semantic_element_id, "non-empty semantic element id")?;
        let mut artifacts = self
            .graph
            .read(|graph| artifacts_for_element_with_inheritance(graph, semantic_element_id))?;
        self.hydrate_artifact_contents(&mut artifacts)?;
        Ok(artifacts)
    }

    /// Reads a semantic artifact by id.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_db_core::{LocalPersistence, SemanticOperation, SemanticPersistence, SemanticResult};
    /// use lumvise_resource_routing::InvocationControl;
    ///
    /// let persistence = LocalPersistence::in_memory().unwrap();
    /// let control = InvocationControl::sixty_seconds();
    /// let result = SemanticPersistence::execute(
    ///     &persistence,
    ///     SemanticOperation::Artifact { artifact_id: "missing".into() },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(result, SemanticResult::Artifact(None)));
    /// ```
    pub fn artifact(&self, artifact_id: &str) -> Result<Option<SemanticArtifact>> {
        require_non_empty(artifact_id, "non-empty semantic artifact id")?;
        let mut artifact = self
            .graph
            .read(|graph| semantic_artifact_by_id(graph, artifact_id));
        if let Some(artifact) = artifact.as_mut() {
            self.hydrate_artifact_contents(std::slice::from_mut(artifact))?;
        }
        Ok(artifact)
    }

    /// Lists every graph-owned semantic artifact without scanning element rows.
    /// Lists artifacts that depend on one semantic element or artifact target.
    pub fn artifact_dependents(
        &self,
        target_kind: &str,
        target_id: &str,
    ) -> Result<Vec<SemanticArtifact>> {
        require_non_empty(target_kind, "non-empty artifact dependency target kind")?;
        require_non_empty(target_id, "non-empty artifact dependency target id")?;
        let mut artifacts = self
            .graph
            .read(|graph| artifact_dependents(graph, target_kind, target_id));
        self.hydrate_artifact_contents(&mut artifacts)?;
        Ok(artifacts)
    }

    /// Lists outgoing relationships for one semantic element.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_db_core::{LocalPersistence, SemanticOperation, SemanticPersistence, SemanticResult};
    /// use lumvise_resource_routing::InvocationControl;
    ///
    /// let persistence = LocalPersistence::in_memory().unwrap();
    /// let control = InvocationControl::sixty_seconds();
    /// let result = SemanticPersistence::execute(
    ///     &persistence,
    ///     SemanticOperation::RelationshipsFrom { semantic_element_id: "missing".into() },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(result, SemanticResult::Relationships(relationships) if relationships.is_empty()));
    /// ```
    pub fn relationships_from(
        &self,
        semantic_element_id: &str,
    ) -> Result<Vec<SemanticRelationship>> {
        require_non_empty(semantic_element_id, "non-empty semantic element id")?;
        Ok(sort_relationships(self.graph.read(|graph| {
            semantic_relationships_from_native_edges(graph, semantic_element_id)
        })))
    }

    /// Lists incoming and outgoing relationships touching the requested elements.
    ///
    /// # Example
    ///
    /// ```
    /// use std::collections::HashSet;
    /// use lumvise_db_core::{LocalPersistence, SemanticOperation, SemanticPersistence, SemanticResult};
    /// use lumvise_resource_routing::InvocationControl;
    ///
    /// let persistence = LocalPersistence::in_memory().unwrap();
    /// let control = InvocationControl::sixty_seconds();
    /// let result = SemanticPersistence::execute(
    ///     &persistence,
    ///     SemanticOperation::RelationshipsTouchingElements { semantic_element_ids: HashSet::new() },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(result, SemanticResult::Relationships(records) if records.is_empty()));
    /// ```
    pub fn relationships_touching_elements(
        &self,
        semantic_element_ids: &HashSet<String>,
    ) -> Result<Vec<SemanticRelationship>> {
        if semantic_element_ids.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self
            .graph
            .read(|graph| semantic_relationships_touching_elements(graph, semantic_element_ids)))
    }

    /// Lists direct semantic artifacts for a batch of element ids.
    ///
    /// # Example
    ///
    /// ```
    /// use std::collections::HashSet;
    /// use lumvise_db_core::{LocalPersistence, SemanticOperation, SemanticPersistence, SemanticResult};
    /// use lumvise_resource_routing::InvocationControl;
    ///
    /// let persistence = LocalPersistence::in_memory().unwrap();
    /// let control = InvocationControl::sixty_seconds();
    /// let result = SemanticPersistence::execute(
    ///     &persistence,
    ///     SemanticOperation::ArtifactsForElements { semantic_element_ids: HashSet::new() },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(result, SemanticResult::Artifacts(records) if records.is_empty()));
    /// ```
    pub fn artifacts_for_elements(
        &self,
        semantic_element_ids: &HashSet<String>,
    ) -> Result<Vec<SemanticArtifact>> {
        if semantic_element_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut artifacts = self
            .graph
            .read(|graph| semantic_artifacts_for_elements(graph, semantic_element_ids));
        self.hydrate_artifact_contents(&mut artifacts)?;
        Ok(artifacts)
    }

    /// Lists semantic elements for one project root.
    ///
    /// # Example
    ///
    /// ```
    /// let db = lumvise_db_core::DbCore::in_memory().unwrap();
    /// assert!(db.storage_manager().semantic_storage().elements_for_project("/repo").unwrap().is_empty());
    /// ```
    #[cfg(test)]
    pub fn elements_for_project(&self, project_root: &str) -> Result<Vec<SemanticElement>> {
        require_non_empty(project_root, "non-empty project root")?;
        let mut elements = self
            .graph
            .read(|graph| semantic_elements_for_project(graph, project_root));
        sort_elements_by_path(&mut elements);
        Ok(elements)
    }

    /// Reads only the requested semantic element ids from one project.
    pub fn elements_by_ids(
        &self,
        project_root: &str,
        semantic_element_ids: &HashSet<String>,
    ) -> Result<Vec<SemanticElement>> {
        require_non_empty(project_root, "non-empty project root")?;
        let mut elements = self.graph.read(|graph| {
            semantic_element_ids
                .iter()
                .filter_map(|id| semantic_element_by_id(graph, id))
                .filter(|element| element.project_root == project_root)
                .collect::<Vec<_>>()
        });
        elements.sort_by(|left, right| left.semantic_element_id.cmp(&right.semantic_element_id));
        Ok(elements)
    }

    pub fn elements_by_ids_including_inactive(
        &self,
        project_root: &str,
        semantic_element_ids: &HashSet<String>,
    ) -> Result<Vec<SemanticElement>> {
        require_non_empty(project_root, "non-empty project root")?;
        let mut elements = self.graph.read(|graph| {
            elements_by_ids_selective(graph, project_root, semantic_element_ids, true)
        });
        elements.sort_by(|left, right| left.semantic_element_id.cmp(&right.semantic_element_id));
        Ok(elements)
    }

    /// Returns the deduplicated union of exact identity-key matches Knowledge's
    /// cross-project inheritance admits candidates through: raw content
    /// fingerprint equality, normalized `(kind, name)`, and normalized
    /// `(kind, file name)`. Spans every project - callers filter by
    /// project scope themselves. Touches the graph only when at least one key
    /// set is non-empty.
    pub fn candidate_source_elements(
        &self,
        content_fingerprints: &HashSet<String>,
        kind_name_keys: &HashSet<String>,
        kind_file_name_keys: &HashSet<String>,
    ) -> Result<Vec<SemanticElement>> {
        if content_fingerprints.is_empty()
            && kind_name_keys.is_empty()
            && kind_file_name_keys.is_empty()
        {
            return Ok(Vec::new());
        }
        let mut elements = self.graph.read(|graph| {
            candidate_elements_by_identity_keys(
                graph,
                content_fingerprints,
                kind_name_keys,
                kind_file_name_keys,
            )
        });
        elements.sort_by(|left, right| left.semantic_element_id.cmp(&right.semantic_element_id));
        Ok(elements)
    }

    pub fn artifacts_by_ids(
        &self,
        artifact_ids: &HashSet<String>,
    ) -> Result<Vec<SemanticArtifact>> {
        if artifact_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut artifacts = self.graph.stable_read(|graph| {
            let mut artifacts = artifacts_by_ids_selective(graph, artifact_ids);
            self.hydrate_artifact_contents(&mut artifacts)?;
            Ok(artifacts)
        })?;
        artifacts.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
        Ok(artifacts)
    }

    pub fn project_artifacts(
        &self,
        project_root: &str,
        artifact_namespace: Option<&str>,
    ) -> Result<Vec<SemanticArtifact>> {
        require_non_empty(project_root, "non-empty project root")?;
        let mut artifacts = self.graph.stable_read(|graph| {
            let mut artifacts =
                project_artifacts_selective(graph, project_root, artifact_namespace);
            self.hydrate_artifact_contents(&mut artifacts)?;
            Ok(artifacts)
        })?;
        artifacts.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
        Ok(artifacts)
    }

    /// Lists project roots currently represented by semantic elements.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_db_core::{LocalPersistence, SemanticOperation, SemanticPersistence, SemanticResult};
    /// use lumvise_resource_routing::InvocationControl;
    ///
    /// let persistence = LocalPersistence::in_memory().unwrap();
    /// let control = InvocationControl::sixty_seconds();
    /// let result = SemanticPersistence::execute(
    ///     &persistence,
    ///     SemanticOperation::ProjectRoots,
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(result, SemanticResult::ProjectRoots(roots) if roots.is_empty()));
    /// ```
    pub fn semantic_project_roots(&self) -> Result<Vec<String>> {
        let roots = self.graph.read(|graph| {
            let store = graph.graph_store();
            let element_ids = store.nodes_by_label("SemanticElement");
            let project_root_key = "project_root".into();
            let active_key = "active".into();
            let project_roots = store.get_node_property_batch(&element_ids, &project_root_key);
            let active = store.get_node_property_batch(&element_ids, &active_key);
            project_roots
                .into_iter()
                .zip(active)
                .filter_map(|(project_root, active)| {
                    if active != Some(GrafeoValue::Bool(true)) {
                        return None;
                    }
                    let Some(GrafeoValue::String(project_root)) = project_root else {
                        return None;
                    };
                    (!project_root.trim().is_empty()).then(|| project_root.to_string())
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        });
        Ok(roots)
    }
}

/// Projection plus an optional still-unpersisted canonical projection. The
/// canonical copy is persisted by the caller after the graph read gate is
/// released, keeping the SQL write mutex out of the graph gate.
type RendererProjectionOutcome = (
    crate::SemanticGraphProjection,
    Option<crate::SemanticGraphProjection>,
);

pub(super) fn canonical_file_projection_request(
    project_root: &str,
) -> crate::SemanticGraphProjectionRequest {
    crate::SemanticGraphProjectionRequest {
        project_root: project_root.to_owned(),
        target_path: None,
        granularity: crate::SemanticGraphGranularity::File,
        recursive: true,
        include_external: true,
        include_first_neighbors: false,
    }
}

pub(super) fn collect_semantic_subtree(
    graph: &grafeo::GrafeoDB,
    root: SemanticElement,
    maximum_depth: usize,
) -> Vec<SemanticElement> {
    crate::local::grafeo::graph_rows::subtree_elements(graph, root, maximum_depth)
}

#[cfg(test)]
pub(super) fn sort_elements_by_path(elements: &mut [SemanticElement]) {
    elements.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.semantic_element_id.cmp(&right.semantic_element_id))
    });
}
