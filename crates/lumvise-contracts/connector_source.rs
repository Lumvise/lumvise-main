//! Native connector folder review records. MCP may request access but cannot decide it.
use crate::{ConnectorGrantState, ConnectorIdentity};
use serde::{Deserialize, Serialize};

/// Restricted asks for new roots; Autoapprove uses only native-selected roots.
/// Example: a newly registered connector starts Restricted.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorSourceMode {
    #[default]
    Restricted,
    Autoapprove,
}

/// Pins the path reviewed by the human to its canonical directory.
/// Example: an alias changing its destination invalidates the original approval.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConnectorSourceFolder {
    pub requested_path: String,
    pub canonical_path: String,
}

/// One read/index decision, including explicit revocation over automatic policy.
/// Example: native Settings revokes a previously allowed folder.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConnectorSourceFolderGrant {
    pub folder: ConnectorSourceFolder,
    pub state: ConnectorGrantState,
}

/// Revision-fenced folder access for one authenticated installation.
/// Example: Settings reads this before submitting a native change.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConnectorSourceAccess {
    pub connector: ConnectorIdentity,
    pub revision: u64,
    pub mode: ConnectorSourceMode,
    pub autoapprove_roots: Vec<ConnectorSourceFolder>,
    pub grants: Vec<ConnectorSourceFolderGrant>,
}

/// Native-only changes; filesystem grants never confer Knowledge writes or administration.
/// Example: submit Folder with the exact folder and revision shown in its popup.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConnectorSourceAccessChange {
    Policy {
        connector_id: String,
        expected_revision: u64,
        mode: ConnectorSourceMode,
        roots: Vec<String>,
    },
    Folder {
        connector_id: String,
        expected_revision: u64,
        folder: ConnectorSourceFolder,
        state: ConnectorGrantState,
    },
}
