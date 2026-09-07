use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub phase: String,
    pub kind: String,
    pub path: String,
    pub bytes: u64,
    #[serde(default)]
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// Stable id for UI selection
    #[serde(default)]
    pub id: String,
}

impl Action {
    pub fn new(
        phase: impl Into<String>,
        kind: impl Into<String>,
        path: impl Into<String>,
        bytes: u64,
        detail: impl Into<String>,
    ) -> Self {
        let phase = phase.into();
        let kind = kind.into();
        let path = path.into();
        let detail = detail.into();
        let id = format!("{phase}:{kind}:{path}");
        Self {
            phase,
            kind,
            path,
            bytes,
            detail,
            command: None,
            id,
        }
    }

    pub fn with_command(mut self, cmd: Vec<String>) -> Self {
        self.command = Some(cmd);
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PhaseResult {
    pub name: String,
    pub reclaimable_bytes: u64,
    pub actions: Vec<Action>,
    pub notes: Vec<String>,
}

impl PhaseResult {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    pub fn add(&mut self, action: Action) {
        self.reclaimable_bytes = self.reclaimable_bytes.saturating_add(action.bytes);
        self.actions.push(action);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyRecord {
    pub action: Action,
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reclaimed_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApplySummary {
    pub files_deleted: u64,
    pub bytes_reclaimed: u64,
    pub failures: u64,
    pub by_phase: Vec<(String, u64)>,
}
