use super::*;
use std::{
    path::Path,
    sync::{Arc, LazyLock, Mutex},
};

static MANAGED_MODEL_STATE_ROOT_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

struct StateRootGuard(Option<std::ffi::OsString>);

impl StateRootGuard {
    fn set(path: &Path) -> Self {
        let previous = std::env::var_os("LUMVISE_STATE_ROOT");
        unsafe {
            std::env::set_var("LUMVISE_STATE_ROOT", path);
        }
        Self(previous)
    }
}

impl Drop for StateRootGuard {
    fn drop(&mut self) {
        unsafe {
            match self.0.take() {
                Some(previous) => std::env::set_var("LUMVISE_STATE_ROOT", previous),
                None => std::env::remove_var("LUMVISE_STATE_ROOT"),
            }
        }
    }
}

#[test]
fn install_managed_models_keeps_app_core_alive_after_restore_failure() {
    let _state_root_lock = MANAGED_MODEL_STATE_ROOT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = tempfile::tempdir().unwrap();
    let models_root = root.path().join("models");
    let published = models_root.join("vector.all-minilm-l6-v2");
    std::fs::create_dir_all(&published).unwrap();
    for relative in [
        "config.json",
        "special_tokens_map.json",
        "tokenizer.json",
        "tokenizer_config.json",
        "model.onnx",
    ] {
        std::fs::write(published.join(relative), b"fixture").unwrap();
    }
    std::fs::write(
        models_root.join(".active-selections.json"),
        br#"{"vector":"vector.all-minilm-l6-v2"}"#,
    )
    .unwrap();
    let _state_root = StateRootGuard::set(root.path());

    let app = Arc::new(AppCore::in_memory().unwrap());
    app.install_managed_models()
        .expect("recoverable model restore must not abort App Core startup");

    let manager = app
        .managed_models()
        .expect("managed model manager must be owned by App Core");
    let status = manager.status_for("vector.all-minilm-l6-v2").unwrap();
    assert_eq!(
        status.state,
        lumvise_neural_core::managed_models::ManagedModelState::Failed
    );
    assert!(!status.active);
}
