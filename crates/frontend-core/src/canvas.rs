use crate::state::FrontendCore;
use crate::{FrontendError, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const CANVAS_HISTORY_LIMIT: usize = 64;

/// Canvas id owned by the desktop whiteboard. Workspace conversation canvases
/// use their session id instead and are discarded when the conversation ends.
pub const MAIN_CANVAS_ID: &str = "main";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasElement {
    pub element_id: String,
    pub element_kind: String,
    pub content: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasPatch {
    pub canvas_id: String,
    pub elements: Vec<CanvasElement>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasRevisionRecord {
    pub revision: u64,
    pub previous_revision: u64,
    pub actor: String,
    pub patch: Value,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasSnapshot {
    pub canvas_id: String,
    pub revision: u64,
    pub elements: Vec<CanvasElement>,
    pub scene: Value,
    pub scene_json: String,
    pub document: Value,
    pub last_updated_by: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revision_history: Vec<CanvasRevisionRecord>,
}

impl CanvasSnapshot {
    pub fn empty(canvas_id: &str) -> Self {
        let scene = empty_excalidraw_scene();
        Self {
            canvas_id: canvas_id.to_string(),
            revision: 0,
            elements: Vec::new(),
            scene_json: scene_json(&scene).unwrap_or_else(|_| "{}".to_string()),
            document: stable_document_from_scene(&scene),
            scene,
            last_updated_by: "system".to_string(),
            updated_at: Utc::now().to_rfc3339(),
            revision_history: Vec::new(),
        }
    }

    pub fn next_element_revision(&self, patch: CanvasPatch) -> Result<Self> {
        validate_canvas_patch(&patch)?;
        let scene = scene_from_canvas_elements(&patch.elements);
        self.next_scene_revision(patch.canvas_id, scene, "user")
    }

    pub fn next_diff_revision(&self, canvas_id: &str, patch: Value, actor: &str) -> Result<Self> {
        require_non_empty(canvas_id, "non-empty canvas id")?;
        require_non_empty(actor, "non-empty canvas actor")?;
        let next_scene = apply_stable_document_patch(&self.scene, patch)?;
        self.next_scene_revision(canvas_id.to_string(), next_scene, actor)
    }

    pub fn next_native_scene_revision(
        &self,
        canvas_id: &str,
        scene: Value,
        actor: &str,
    ) -> Result<Self> {
        require_non_empty(canvas_id, "non-empty canvas id")?;
        require_non_empty(actor, "non-empty canvas actor")?;
        self.next_scene_revision(canvas_id.to_string(), scene, actor)
    }

    pub fn user_changes(&self, canvas_id: &str, since_revision: u64) -> Result<Value> {
        require_non_empty(canvas_id, "non-empty canvas id")?;
        if self.canvas_id != canvas_id {
            return Err(FrontendError::MissingValue {
                value: canvas_id.to_string(),
                expected: "known canvas id".to_string(),
            });
        }
        validate_requested_revision(self, since_revision)?;
        let changes = self
            .revision_history
            .iter()
            .filter(|record| record.revision > since_revision && record.actor == "user")
            .collect::<Vec<_>>();
        Ok(json!({
            "canvas_id": self.canvas_id,
            "since_revision": since_revision,
            "current_revision": self.revision,
            "has_changes": !changes.is_empty(),
            "changes": changes
        }))
    }

    fn next_scene_revision(&self, canvas_id: String, scene: Value, actor: &str) -> Result<Self> {
        let scene = scene_with_actor_text_normalized(scene, actor);
        validate_scene(&scene)?;
        let updated_at = Utc::now().to_rfc3339();
        let record = self.revision_record(&scene, actor, &updated_at);
        let mut revision_history = self.revision_history.clone();
        revision_history.push(record);
        trim_canvas_history(&mut revision_history);
        Ok(Self {
            canvas_id,
            revision: self.revision + 1,
            elements: canvas_elements_from_scene(&scene),
            scene_json: scene_json(&scene)?,
            document: stable_document_from_scene(&scene),
            scene,
            last_updated_by: actor.to_string(),
            updated_at,
            revision_history,
        })
    }

    fn revision_record(
        &self,
        scene: &Value,
        actor: &str,
        updated_at: &str,
    ) -> CanvasRevisionRecord {
        CanvasRevisionRecord {
            revision: self.revision + 1,
            previous_revision: self.revision,
            actor: actor.to_string(),
            patch: diff_stable_documents(&self.scene, scene),
            updated_at: updated_at.to_string(),
        }
    }
}

pub fn empty_excalidraw_scene() -> Value {
    json!({
        "type": "excalidraw",
        "version": 2,
        "elements": [],
        "appState": {},
        "files": {}
    })
}

fn scene_json(scene: &Value) -> Result<String> {
    serde_json::to_string(scene)
        .map_err(|error| FrontendError::invalid_value(error.to_string(), "Excalidraw scene JSON"))
}

fn validate_canvas_patch(patch: &CanvasPatch) -> Result<()> {
    require_non_empty(&patch.canvas_id, "non-empty canvas id")?;
    for element in &patch.elements {
        require_non_empty(&element.element_id, "non-empty canvas element id")?;
        require_non_empty(&element.element_kind, "non-empty canvas element kind")?;
    }
    Ok(())
}

fn validate_scene(scene: &Value) -> Result<()> {
    if !scene.is_object() {
        return Err(FrontendError::invalid_value(
            scene.to_string(),
            "Excalidraw scene object",
        ));
    }
    if !scene.get("elements").is_none_or(Value::is_array) {
        return Err(FrontendError::invalid_value(
            scene.to_string(),
            "Excalidraw scene elements array",
        ));
    }
    Ok(())
}

fn scene_with_actor_text_normalized(scene: Value, actor: &str) -> Value {
    if actor != "assistant" {
        return scene;
    }
    scene_with_literal_line_breaks(scene)
}

fn scene_with_literal_line_breaks(mut scene: Value) -> Value {
    let Some(elements) = scene.get_mut("elements").and_then(Value::as_array_mut) else {
        return scene;
    };
    for element in elements {
        normalize_text_element_line_breaks(element);
    }
    scene
}

fn normalize_text_element_line_breaks(element: &mut Value) {
    let Some(object) = element.as_object_mut() else {
        return;
    };
    if string_field_from_map(object, "type").as_deref() != Some("text") {
        return;
    }
    for field in ["text", "originalText", "rawText"] {
        normalize_text_field_line_breaks(object, field);
    }
}

fn normalize_text_field_line_breaks(object: &mut serde_json::Map<String, Value>, field: &str) {
    let Some(text) = object.get(field).and_then(Value::as_str) else {
        return;
    };
    let normalized = text
        .replace("\\r\\n", "\n")
        .replace("\\n", "\n")
        .replace("\\r", "\n");
    if normalized != text {
        object.insert(field.to_string(), Value::String(normalized));
    }
}

fn scene_from_canvas_elements(elements: &[CanvasElement]) -> Value {
    json!({
        "type": "excalidraw",
        "version": 2,
        "elements": elements.iter().map(native_element_from_canvas_element).collect::<Vec<_>>(),
        "appState": {},
        "files": {}
    })
}

fn canvas_elements_from_scene(scene: &Value) -> Vec<CanvasElement> {
    scene
        .get("elements")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, element)| canvas_element_from_native_element(element, index))
        .collect()
}

fn stable_document_from_scene(scene: &Value) -> Value {
    let mut elements_by_id = serde_json::Map::new();
    let mut element_order = Vec::new();
    for element in scene_elements(scene) {
        let Some(id) = element.get("id").and_then(Value::as_str) else {
            continue;
        };
        elements_by_id.insert(id.to_string(), element.clone());
        element_order.push(Value::String(id.to_string()));
    }
    json!({
        "type": scene.get("type").cloned().unwrap_or_else(|| json!("excalidraw")),
        "version": scene.get("version").cloned().unwrap_or_else(|| json!(2)),
        "elementsById": elements_by_id,
        "elementOrder": element_order,
        "appState": scene.get("appState").cloned().unwrap_or_else(|| json!({})),
        "files": scene.get("files").cloned().unwrap_or_else(|| json!({}))
    })
}

fn scene_from_stable_document(document: &Value) -> Value {
    let elements_by_id = document
        .get("elementsById")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let elements = document
        .get("elementOrder")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|id| elements_by_id.get(id.as_str()?).cloned())
        .collect::<Vec<_>>();
    json!({
        "type": document.get("type").cloned().unwrap_or_else(|| json!("excalidraw")),
        "version": document.get("version").cloned().unwrap_or_else(|| json!(2)),
        "elements": elements,
        "appState": document.get("appState").cloned().unwrap_or_else(|| json!({})),
        "files": document.get("files").cloned().unwrap_or_else(|| json!({}))
    })
}

fn apply_stable_document_patch(scene: &Value, patch_value: Value) -> Result<Value> {
    let patch_value = normalize_stable_document_patch_value(patch_value);
    let patch =
        serde_json::from_value::<json_patch::Patch>(patch_value.clone()).map_err(|error| {
            FrontendError::invalid_value(
                format!("{patch_value}: {error}"),
                "RFC 6902 JSON Patch array",
            )
        })?;
    let mut document = stable_document_from_scene(scene);
    json_patch::patch(&mut document, &patch).map_err(|error| {
        FrontendError::invalid_value(
            format!("{document}: {error}"),
            "patchable Excalidraw canvas document",
        )
    })?;
    let next_scene = scene_from_stable_document(&document);
    validate_scene(&next_scene)?;
    Ok(next_scene)
}

fn normalize_stable_document_patch_value(value: Value) -> Value {
    let Value::Array(operations) = value else {
        return value;
    };
    Value::Array(
        operations
            .into_iter()
            .map(normalize_stable_document_patch_operation)
            .collect(),
    )
}

fn normalize_stable_document_patch_operation(mut operation: Value) -> Value {
    let Some(object) = operation.as_object_mut() else {
        return operation;
    };
    normalize_stable_document_pointer_field(object, "path");
    normalize_stable_document_pointer_field(object, "from");
    operation
}

fn normalize_stable_document_pointer_field(map: &mut serde_json::Map<String, Value>, field: &str) {
    let Some(Value::String(pointer)) = map.get_mut(field) else {
        return;
    };
    if let Some(normalized) = normalize_stable_document_pointer(pointer) {
        *pointer = normalized;
    }
}

fn normalize_stable_document_pointer(pointer: &str) -> Option<String> {
    let rest = pointer.strip_prefix("/document")?;
    if !rest.is_empty() && !rest.starts_with('/') {
        return None;
    }
    Some(rest.to_string())
}

fn diff_stable_documents(previous_scene: &Value, next_scene: &Value) -> Value {
    let previous = stable_document_from_scene(previous_scene);
    let next = stable_document_from_scene(next_scene);
    serde_json::to_value(json_patch::diff(&previous, &next)).unwrap_or_else(|_| json!([]))
}

fn native_element_from_canvas_element(element: &CanvasElement) -> Value {
    if element.content.get("id").is_some() && element.content.get("type").is_some() {
        return element.content.clone();
    }
    let mut native = default_native_element_from_canvas_element(element);
    merge_canvas_element_content(&mut native, &element.content);
    native
}

fn default_native_element_from_canvas_element(element: &CanvasElement) -> Value {
    json!({
        "id": element.element_id,
        "type": element.element_kind,
        "x": 0,
        "y": 0,
        "width": 100,
        "height": 80
    })
}

fn merge_canvas_element_content(native: &mut Value, content: &Value) {
    let Some(native_object) = native.as_object_mut() else {
        return;
    };
    let Some(content_object) = content.as_object() else {
        return;
    };
    for (key, value) in content_object {
        if key == "id" || key == "type" {
            continue;
        }
        native_object.insert(key.clone(), value.clone());
    }
}

fn canvas_element_from_native_element(element: &Value, index: usize) -> CanvasElement {
    CanvasElement {
        element_id: string_field(element, "id")
            .unwrap_or_else(|| format!("native-element-{}", index + 1)),
        element_kind: string_field(element, "type").unwrap_or_else(|| "unknown".to_string()),
        content: element.clone(),
    }
}

fn scene_elements(scene: &Value) -> Vec<Value> {
    scene
        .get("elements")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn validate_requested_revision(canvas: &CanvasSnapshot, since_revision: u64) -> Result<()> {
    if since_revision > canvas.revision {
        return Err(FrontendError::invalid_value(
            since_revision.to_string(),
            "canvas revision not newer than current revision",
        ));
    }
    let Some(oldest) = canvas.revision_history.first() else {
        return Ok(());
    };
    if since_revision < oldest.previous_revision {
        return Err(FrontendError::invalid_value(
            since_revision.to_string(),
            "canvas revision still available in history",
        ));
    }
    Ok(())
}

fn trim_canvas_history(history: &mut Vec<CanvasRevisionRecord>) {
    if history.len() <= CANVAS_HISTORY_LIMIT {
        return;
    }
    history.drain(0..history.len() - CANVAS_HISTORY_LIMIT);
}

fn require_non_empty(value: &str, expected: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(FrontendError::invalid_value(value, expected));
    }
    Ok(())
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn string_field_from_map(map: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

impl FrontendCore {
    fn canvas_or_empty(&self, canvas_id: &str) -> CanvasSnapshot {
        self.canvases
            .get(canvas_id)
            .cloned()
            .unwrap_or_else(|| CanvasSnapshot::empty(canvas_id))
    }

    /// Updates one canvas from an element patch.
    ///
    /// `CanvasPatch` already carries `canvas_id`, so it stays the single
    /// source of truth: the `canvas_id` argument must match it and a
    /// mismatch is rejected.
    ///
    /// # Example
    ///
    /// ```
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// let patch = lumvise_frontend_core::CanvasPatch { canvas_id: "main".into(), elements: vec![] };
    /// assert_eq!(core.update_canvas("main", patch).unwrap().revision, 1);
    /// ```
    pub fn update_canvas(&mut self, canvas_id: &str, patch: CanvasPatch) -> Result<CanvasSnapshot> {
        if patch.canvas_id != canvas_id {
            return Err(FrontendError::invalid_value(
                patch.canvas_id.clone(),
                format!("canvas id matching the update target {canvas_id:?}"),
            ));
        }
        let next_canvas = self
            .canvas_or_empty(canvas_id)
            .next_element_revision(patch)?;
        self.canvases
            .insert(canvas_id.to_string(), next_canvas.clone());
        Ok(next_canvas)
    }

    /// Applies an RFC 6902 patch to one canvas's Excalidraw document.
    ///
    /// # Example
    ///
    /// ```
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// let patch = serde_json::json!([
    ///   { "op": "add", "path": "/elementsById/card", "value": { "id": "card", "type": "rectangle" } },
    ///   { "op": "add", "path": "/elementOrder/0", "value": "card" }
    /// ]);
    /// assert_eq!(core.apply_canvas_diff("main", patch, "assistant", Some(0)).unwrap().revision, 1);
    /// ```
    pub fn apply_canvas_diff(
        &mut self,
        canvas_id: &str,
        patch: serde_json::Value,
        actor: &str,
        expected_revision: Option<u64>,
    ) -> Result<CanvasSnapshot> {
        let current = self.canvas_or_empty(canvas_id);
        if expected_revision.is_some_and(|revision| revision != current.revision) {
            return Err(FrontendError::InvalidValue {
                value: format!("base revision {expected_revision:?}"),
                expected: format!("current canvas revision {}", current.revision),
            });
        }
        let next_canvas = current.next_diff_revision(canvas_id, patch, actor)?;
        self.canvases
            .insert(canvas_id.to_string(), next_canvas.clone());
        Ok(next_canvas)
    }

    /// Replaces one canvas's Excalidraw scene and records a revision diff.
    ///
    /// # Example
    ///
    /// ```
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// let scene = serde_json::json!({ "type": "excalidraw", "version": 2, "elements": [] });
    /// assert_eq!(core.replace_canvas_scene("main", scene, "user").unwrap().revision, 1);
    /// ```
    pub fn replace_canvas_scene(
        &mut self,
        canvas_id: &str,
        scene: serde_json::Value,
        actor: &str,
    ) -> Result<CanvasSnapshot> {
        let next_canvas = self
            .canvas_or_empty(canvas_id)
            .next_native_scene_revision(canvas_id, scene, actor)?;
        self.canvases
            .insert(canvas_id.to_string(), next_canvas.clone());
        Ok(next_canvas)
    }

    /// Returns one canvas's snapshot. Unknown ids read as an empty snapshot
    /// for that id and are not inserted.
    ///
    /// # Example
    ///
    /// ```
    /// let core = lumvise_frontend_core::FrontendCore::default();
    /// assert_eq!(core.canvas("main").revision, 0);
    /// assert_eq!(core.canvas("workspace:3f").canvas_id, "workspace:3f");
    /// ```
    pub fn canvas(&self, canvas_id: &str) -> CanvasSnapshot {
        self.canvas_or_empty(canvas_id)
    }

    /// Returns user-authored canvas diffs after a known revision.
    ///
    /// # Example
    ///
    /// ```
    /// let core = lumvise_frontend_core::FrontendCore::default();
    /// assert_eq!(core.user_canvas_changes("main", 0).unwrap()["has_changes"], serde_json::json!(false));
    /// ```
    pub fn user_canvas_changes(
        &self,
        canvas_id: &str,
        since_revision: u64,
    ) -> Result<serde_json::Value> {
        self.canvas_or_empty(canvas_id)
            .user_changes(canvas_id, since_revision)
    }

    /// Removes a non-main canvas and returns whether it existed. The main
    /// canvas cannot be discarded.
    ///
    /// # Example
    ///
    /// ```
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// let scene = serde_json::json!({ "type": "excalidraw", "version": 2, "elements": [] });
    /// core.replace_canvas_scene("workspace:3f", scene, "user").unwrap();
    /// assert!(core.discard_canvas("workspace:3f").unwrap());
    /// assert!(!core.discard_canvas("workspace:3f").unwrap());
    /// assert!(core.discard_canvas(lumvise_frontend_core::MAIN_CANVAS_ID).is_err());
    /// ```
    pub fn discard_canvas(&mut self, canvas_id: &str) -> Result<bool> {
        if canvas_id == MAIN_CANVAS_ID {
            return Err(FrontendError::invalid_value(
                canvas_id,
                "non-main canvas id",
            ));
        }
        Ok(self.canvases.remove(canvas_id).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn assistant_diff_turns_literal_slash_n_into_line_breaks() {
        let canvas = CanvasSnapshot::empty("main");
        let patch = json!([
            {
                "op": "add",
                "path": "/elementsById/card-text",
                "value": {
                    "id": "card-text",
                    "type": "text",
                    "text": "Identity\\nDavid: CEO",
                    "originalText": "Identity\\nDavid: CEO"
                }
            },
            { "op": "add", "path": "/elementOrder/0", "value": "card-text" }
        ]);

        let next = canvas
            .next_diff_revision("main", patch, "assistant")
            .unwrap();
        let text = next.scene["elements"][0]["text"].as_str().unwrap();
        let original_text = next.scene["elements"][0]["originalText"].as_str().unwrap();

        assert_eq!(text, "Identity\nDavid: CEO");
        assert_eq!(original_text, "Identity\nDavid: CEO");
    }

    #[test]
    fn assistant_diff_accepts_document_prefixed_stable_paths() {
        let canvas = CanvasSnapshot::empty("main");
        let patch = json!([
            {
                "op": "add",
                "path": "/document/elementsById/card",
                "value": { "id": "card", "type": "rectangle", "x": 10, "y": 20 }
            },
            { "op": "add", "path": "/document/elementOrder/0", "value": "card" }
        ]);

        let next = canvas
            .next_diff_revision("main", patch, "assistant")
            .unwrap();

        assert_eq!(next.scene["elements"][0]["id"].as_str().unwrap(), "card");
        assert_eq!(
            next.document["elementsById"]["card"]["type"]
                .as_str()
                .unwrap(),
            "rectangle"
        );
    }

    #[test]
    fn user_scene_keeps_literal_slash_n_text() {
        let canvas = CanvasSnapshot::empty("main");
        let scene = json!({
            "type": "excalidraw",
            "version": 2,
            "elements": [{
                "id": "user-text",
                "type": "text",
                "text": "Use \\n when discussing escapes"
            }]
        });

        let next = canvas
            .next_native_scene_revision("main", scene, "user")
            .unwrap();

        assert_eq!(
            next.scene["elements"][0]["text"].as_str().unwrap(),
            "Use \\n when discussing escapes"
        );
    }

    fn scene_with_element(id: &str) -> serde_json::Value {
        json!({
            "type": "excalidraw",
            "version": 2,
            "elements": [{ "id": id, "type": "rectangle" }]
        })
    }

    #[test]
    fn workspace_canvas_diffs_leave_main_untouched() {
        let mut core = FrontendCore::default();
        let workspace = core
            .replace_canvas_scene("workspace:3f", scene_with_element("ws-card"), "assistant")
            .unwrap();
        assert_eq!(workspace.revision, 1);

        let main = core.canvas(MAIN_CANVAS_ID);
        assert_eq!(main.revision, 0);
        assert!(main.elements.is_empty());
        assert!(
            main.document["elementsById"].get("ws-card").is_none(),
            "main canvas must not see workspace elements"
        );

        // Revisions advance independently per canvas.
        let main_updated = core
            .replace_canvas_scene(MAIN_CANVAS_ID, scene_with_element("main-card"), "user")
            .unwrap();
        assert_eq!(main_updated.revision, 1);
        assert_eq!(core.canvas("workspace:3f").revision, 1);
        assert!(
            core.canvas("workspace:3f").document["elementsById"]
                .get("main-card")
                .is_none()
        );
    }

    #[test]
    fn update_canvas_rejects_patch_id_mismatch() {
        let mut core = FrontendCore::default();
        let patch = CanvasPatch {
            canvas_id: "workspace:3f".into(),
            elements: vec![],
        };
        let error = core
            .update_canvas(MAIN_CANVAS_ID, patch)
            .expect_err("canvas id mismatch");
        assert!(matches!(error, FrontendError::InvalidValue { .. }));
        assert_eq!(core.canvas(MAIN_CANVAS_ID).revision, 0);
    }

    #[test]
    fn discard_removes_only_target_canvas_and_never_main() {
        let mut core = FrontendCore::default();
        core.replace_canvas_scene("workspace:3f", scene_with_element("a"), "user")
            .unwrap();
        core.replace_canvas_scene("workspace:7a", scene_with_element("b"), "user")
            .unwrap();

        assert!(core.discard_canvas("workspace:3f").unwrap());
        assert!(!core.discard_canvas("workspace:3f").unwrap());
        assert_eq!(core.canvas("workspace:3f").revision, 0);
        assert_eq!(core.canvas("workspace:7a").revision, 1);

        assert!(core.discard_canvas(MAIN_CANVAS_ID).is_err());
        assert_eq!(core.canvas(MAIN_CANVAS_ID).canvas_id, MAIN_CANVAS_ID);
    }

    #[test]
    fn unknown_canvas_id_reads_empty_without_inserting() {
        let mut core = FrontendCore::default();
        let unknown = core.canvas("workspace:gone");
        assert_eq!(unknown.revision, 0);
        assert_eq!(unknown.canvas_id, "workspace:gone");

        core.replace_canvas_scene(MAIN_CANVAS_ID, scene_with_element("m"), "user")
            .unwrap();
        // The earlier empty read never created an entry.
        assert!(!core.discard_canvas("workspace:gone").unwrap());
    }
}
