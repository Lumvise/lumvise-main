use crate::error::{FrontendError, Result};
use crate::state::{FrontendCore, FrontendRuntimeSnapshot};
use crate::types::{ModalityStreamKind, ModalityStreamPhase};
use serde::{Deserialize, Serialize};

impl FrontendCore {
    /// Updates one modality stream state visible to the dashboard.
    ///
    /// # Example
    ///
    /// ```
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// core.spawn_app(lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0)).unwrap();
    /// core.set_modality_stream(
    ///     lumvise_frontend_core::ModalityStreamKind::Speech,
    ///     lumvise_frontend_core::ModalityStreamPhase::Capturing,
    ///     None,
    /// ).unwrap();
    /// assert_eq!(core.state().streams.speech.phase, lumvise_frontend_core::ModalityStreamPhase::Capturing);
    /// ```
    pub fn set_modality_stream(
        &mut self,
        kind: ModalityStreamKind,
        phase: ModalityStreamPhase,
        error: Option<String>,
    ) -> Result<FrontendRuntimeSnapshot> {
        self.require_spawned()?;
        self.state.streams = self.state.streams.clone().with_stream(kind, phase, error)?;
        self.snapshot()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModalityStreamState {
    pub kind: ModalityStreamKind,
    pub phase: ModalityStreamPhase,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModalityStreamCatalog {
    pub speech: ModalityStreamState,
    pub screenshots: ModalityStreamState,
    pub desktop_broadcasts: ModalityStreamState,
    pub screen_frame_broadcasts: ModalityStreamState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModalityStreamsInterface {
    pub streams: Vec<ModalityStreamState>,
}

impl ModalityStreamState {
    /// Creates a modality stream state in the ready phase.
    ///
    /// # Example
    ///
    /// ```
    /// let stream = lumvise_frontend_core::ModalityStreamState::ready(
    ///     lumvise_frontend_core::ModalityStreamKind::Speech,
    /// );
    /// assert_eq!(stream.last_error, None);
    /// ```
    pub fn ready(kind: ModalityStreamKind) -> Self {
        Self {
            kind,
            phase: ModalityStreamPhase::Ready,
            last_error: None,
        }
    }

    /// Returns the same stream with a changed phase and validated error state.
    ///
    /// # Example
    ///
    /// ```
    /// let stream = lumvise_frontend_core::ModalityStreamState::ready(
    ///     lumvise_frontend_core::ModalityStreamKind::Speech,
    /// ).with_phase(lumvise_frontend_core::ModalityStreamPhase::Capturing, None).unwrap();
    /// assert_eq!(stream.phase, lumvise_frontend_core::ModalityStreamPhase::Capturing);
    /// ```
    pub fn with_phase(
        self,
        phase: ModalityStreamPhase,
        last_error: Option<String>,
    ) -> Result<Self> {
        if phase == ModalityStreamPhase::Failed && last_error.as_deref().unwrap_or("").is_empty() {
            return Err(FrontendError::invalid_value("", "non-empty stream failure"));
        }
        Ok(Self {
            phase,
            last_error,
            ..self
        })
    }
}

impl Default for ModalityStreamCatalog {
    fn default() -> Self {
        Self {
            speech: ModalityStreamState::ready(ModalityStreamKind::Speech),
            screenshots: ModalityStreamState::ready(ModalityStreamKind::Screenshots),
            desktop_broadcasts: ModalityStreamState::ready(ModalityStreamKind::DesktopBroadcasts),
            screen_frame_broadcasts: ModalityStreamState::ready(
                ModalityStreamKind::ScreenFrameBroadcasts,
            ),
        }
    }
}

impl ModalityStreamCatalog {
    /// Updates one modality stream by kind.
    ///
    /// # Example
    ///
    /// ```
    /// let catalog = lumvise_frontend_core::ModalityStreamCatalog::default()
    ///     .with_stream(
    ///         lumvise_frontend_core::ModalityStreamKind::Speech,
    ///         lumvise_frontend_core::ModalityStreamPhase::Capturing,
    ///         None,
    ///     )
    ///     .unwrap();
    /// assert_eq!(catalog.speech.phase, lumvise_frontend_core::ModalityStreamPhase::Capturing);
    /// ```
    pub fn with_stream(
        mut self,
        kind: ModalityStreamKind,
        phase: ModalityStreamPhase,
        last_error: Option<String>,
    ) -> Result<Self> {
        match kind {
            ModalityStreamKind::Speech => {
                self.speech = self.speech.with_phase(phase, last_error)?
            }
            ModalityStreamKind::Screenshots => {
                self.screenshots = self.screenshots.with_phase(phase, last_error)?;
            }
            ModalityStreamKind::DesktopBroadcasts => {
                self.desktop_broadcasts = self.desktop_broadcasts.with_phase(phase, last_error)?;
            }
            ModalityStreamKind::ScreenFrameBroadcasts => {
                self.screen_frame_broadcasts =
                    self.screen_frame_broadcasts.with_phase(phase, last_error)?;
            }
        }
        Ok(self)
    }

    pub fn get(&self, kind: ModalityStreamKind) -> &ModalityStreamState {
        match kind {
            ModalityStreamKind::Speech => &self.speech,
            ModalityStreamKind::Screenshots => &self.screenshots,
            ModalityStreamKind::DesktopBroadcasts => &self.desktop_broadcasts,
            ModalityStreamKind::ScreenFrameBroadcasts => &self.screen_frame_broadcasts,
        }
    }

    pub fn all(&self) -> [ModalityStreamState; 4] {
        [
            self.speech.clone(),
            self.screenshots.clone(),
            self.desktop_broadcasts.clone(),
            self.screen_frame_broadcasts.clone(),
        ]
    }

    pub fn interface(&self) -> ModalityStreamsInterface {
        ModalityStreamsInterface {
            streams: self.all().into_iter().collect(),
        }
    }
}
