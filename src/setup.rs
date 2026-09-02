//! Pure first-run setup/onboarding state.
//!
//! The graphical onboarding surface can render this state machine without
//! owning persistence or process execution. Each transition is deterministic,
//! failed steps remain resumable, and persistence is a versioned text format
//! that the platform layer can write atomically.

use std::{
    fmt,
    path::{Path, PathBuf},
};

/// Current on-disk schema version.
pub const SETUP_SCHEMA_VERSION: u32 = 1;
/// Default persisted state filename under the user's state directory.
pub const SETUP_STATE_FILENAME: &str = "setup.state";
const PERSISTENCE_HEADER: &str = "rouch-setup-v1";

/// Ordered stages of the initial Rouch environment setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SetupStep {
    /// Explain the setup and collect the user's first confirmation.
    #[default]
    Welcome,
    /// Verify/install the minimum runtime and graphics dependencies.
    Dependencies,
    /// Probe Vulkan and retain OpenGL as the fallback.
    GraphicsBackend,
    /// Offer the recommended, bounded swapfile configuration.
    Swapfile,
    /// Install the freedesktop application entry.
    DesktopEntry,
    /// Offer enabling Rouch at session start.
    Autostart,
    /// Download/activate only the first interface stage.
    InterfaceStage1,
    /// Terminal state after every actionable stage is complete or skipped.
    Complete,
}

/// Alias for integrations that call a stage an installation step.
pub type SetupStage = SetupStep;

impl SetupStep {
    /// All actionable steps, in order.
    pub const fn actionable() -> [Self; 7] {
        [
            Self::Welcome,
            Self::Dependencies,
            Self::GraphicsBackend,
            Self::Swapfile,
            Self::DesktopEntry,
            Self::Autostart,
            Self::InterfaceStage1,
        ]
    }

    /// Whether this step can be skipped in the onboarding UI.
    pub const fn is_optional(self) -> bool {
        matches!(self, Self::Swapfile | Self::Autostart)
    }

    /// Stable key used by persistence and diagnostics.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Welcome => "welcome",
            Self::Dependencies => "dependencies",
            Self::GraphicsBackend => "graphics",
            Self::Swapfile => "swapfile",
            Self::DesktopEntry => "desktop-entry",
            Self::Autostart => "autostart",
            Self::InterfaceStage1 => "interface-stage1",
            Self::Complete => "complete",
        }
    }

    /// The next ordered step, or None after the final stage.
    pub const fn next(self) -> Option<Self> {
        match self {
            Self::Welcome => Some(Self::Dependencies),
            Self::Dependencies => Some(Self::GraphicsBackend),
            Self::GraphicsBackend => Some(Self::Swapfile),
            Self::Swapfile => Some(Self::DesktopEntry),
            Self::DesktopEntry => Some(Self::Autostart),
            Self::Autostart => Some(Self::InterfaceStage1),
            Self::InterfaceStage1 => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    fn from_key(value: &str) -> Option<Self> {
        Some(match value {
            "welcome" => Self::Welcome,
            "dependencies" => Self::Dependencies,
            "graphics" => Self::GraphicsBackend,
            "swapfile" => Self::Swapfile,
            "desktop-entry" => Self::DesktopEntry,
            "autostart" => Self::Autostart,
            "interface-stage1" => Self::InterfaceStage1,
            "complete" => Self::Complete,
            _ => return None,
        })
    }
}

impl fmt::Display for SetupStep {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.key())
    }
}

/// Lifecycle state of the onboarding process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SetupStatus {
    /// No setup action has been started.
    #[default]
    NotStarted,
    /// The current step can be completed, skipped, or failed.
    InProgress,
    /// The current step failed and can be retried without losing progress.
    Failed,
    /// All stages have been completed or explicitly skipped.
    Complete,
}

impl SetupStatus {
    fn key(self) -> &'static str {
        match self {
            Self::NotStarted => "not-started",
            Self::InProgress => "in-progress",
            Self::Failed => "failed",
            Self::Complete => "complete",
        }
    }

    fn from_key(value: &str) -> Option<Self> {
        Some(match value {
            "not-started" => Self::NotStarted,
            "in-progress" => Self::InProgress,
            "failed" => Self::Failed,
            "complete" => Self::Complete,
            _ => return None,
        })
    }
}

/// Serializable state consumed by the onboarding UI and integration layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupState {
    /// Schema version written to disk.
    pub schema_version: u32,
    /// Lifecycle status.
    pub status: SetupStatus,
    /// Step currently displayed.
    pub current_step: SetupStep,
    /// Steps that completed successfully.
    pub completed: Vec<SetupStep>,
    /// Optional steps deliberately skipped by the user.
    pub skipped: Vec<SetupStep>,
    /// Number of setup attempts, including retries.
    pub attempts: u32,
    /// Error shown by the recovery UI, if the current step failed.
    pub last_error: Option<String>,
}

impl Default for SetupState {
    fn default() -> Self {
        Self::new()
    }
}

impl SetupState {
    /// Construct a fresh, not-yet-started setup.
    pub const fn new() -> Self {
        Self {
            schema_version: SETUP_SCHEMA_VERSION,
            status: SetupStatus::NotStarted,
            current_step: SetupStep::Welcome,
            completed: Vec::new(),
            skipped: Vec::new(),
            attempts: 0,
            last_error: None,
        }
    }

    /// True when the onboarding sequence has reached its terminal stage.
    pub const fn is_complete(&self) -> bool {
        matches!(self.status, SetupStatus::Complete)
    }

    /// Start a fresh setup attempt.
    pub fn start(&mut self) -> Result<SetupTransition, SetupError> {
        match self.status {
            SetupStatus::NotStarted => {
                let previous = self.snapshot();
                self.status = SetupStatus::InProgress;
                self.attempts = self.attempts.saturating_add(1);
                Ok(self.transition_from(previous))
            }
            SetupStatus::InProgress => Ok(SetupTransition::unchanged(self)),
            SetupStatus::Failed => Err(SetupError::RecoverBeforeStarting),
            SetupStatus::Complete => Err(SetupError::AlreadyComplete),
        }
    }

    /// Mark the current step complete and advance to the next stage.
    pub fn complete_current(&mut self) -> Result<SetupTransition, SetupError> {
        self.ensure_in_progress("complete")?;
        let previous = self.snapshot();
        let current = self.current_step;
        push_unique(&mut self.completed, current);
        self.skipped.retain(|step| *step != current);
        self.last_error = None;
        self.advance_after_current();
        Ok(self.transition_from(previous))
    }

    /// Skip the current optional stage and advance.
    pub fn skip_current(&mut self) -> Result<SetupTransition, SetupError> {
        self.ensure_in_progress("skip")?;
        if !self.current_step.is_optional() {
            return Err(SetupError::CannotSkip {
                step: self.current_step,
            });
        }
        let previous = self.snapshot();
        push_unique(&mut self.skipped, self.current_step);
        self.completed.retain(|step| *step != self.current_step);
        self.last_error = None;
        self.advance_after_current();
        Ok(self.transition_from(previous))
    }

    /// Record a failure while retaining the current step for recovery.
    pub fn fail(&mut self, message: impl Into<String>) -> Result<SetupTransition, SetupError> {
        self.ensure_in_progress("fail")?;
        let message = message.into();
        if message.trim().is_empty() {
            return Err(SetupError::EmptyFailure);
        }
        let previous = self.snapshot();
        self.status = SetupStatus::Failed;
        self.last_error = Some(message);
        Ok(self.transition_from(previous))
    }

    /// Recover a failed step without discarding completed work.
    pub fn recover(&mut self) -> Result<SetupTransition, SetupError> {
        match self.status {
            SetupStatus::Failed => {
                let previous = self.snapshot();
                self.status = SetupStatus::InProgress;
                self.last_error = None;
                self.attempts = self.attempts.saturating_add(1);
                Ok(self.transition_from(previous))
            }
            SetupStatus::InProgress => Ok(SetupTransition::unchanged(self)),
            SetupStatus::NotStarted => Err(SetupError::NotStarted),
            SetupStatus::Complete => Err(SetupError::AlreadyComplete),
        }
    }

    /// Apply a UI action to the state machine.
    pub fn apply(&mut self, action: SetupAction) -> Result<SetupTransition, SetupError> {
        match action {
            SetupAction::Start => self.start(),
            SetupAction::Complete => self.complete_current(),
            SetupAction::Skip => self.skip_current(),
            SetupAction::Fail(message) => self.fail(message),
            SetupAction::Recover => self.recover(),
        }
    }

    fn ensure_in_progress(&self, action: &'static str) -> Result<(), SetupError> {
        match self.status {
            SetupStatus::InProgress => Ok(()),
            SetupStatus::NotStarted => Err(SetupError::NotStarted),
            SetupStatus::Failed => Err(SetupError::RecoverBeforeAction { action }),
            SetupStatus::Complete => Err(SetupError::AlreadyComplete),
        }
    }

    fn advance_after_current(&mut self) {
        self.current_step = self.current_step.next().unwrap_or(SetupStep::Complete);
        if self.current_step == SetupStep::Complete {
            self.status = SetupStatus::Complete;
        }
    }

    fn snapshot(&self) -> SetupSnapshot {
        SetupSnapshot {
            status: self.status,
            step: self.current_step,
        }
    }

    fn transition_from(&self, previous: SetupSnapshot) -> SetupTransition {
        SetupTransition {
            previous_status: previous.status,
            status: self.status,
            previous_step: previous.step,
            step: self.current_step,
            changed: previous.status != self.status || previous.step != self.current_step,
        }
    }

    /// Validate invariants before persisting or handing state to a renderer.
    pub fn validate(&self) -> Result<(), SetupDecodeError> {
        if self.schema_version != SETUP_SCHEMA_VERSION {
            return Err(SetupDecodeError::InvalidState(format!(
                "unsupported schema version {}",
                self.schema_version
            )));
        }
        let actionable = SetupStep::actionable();
        for step in self.completed.iter().chain(self.skipped.iter()) {
            if !actionable.contains(step) {
                return Err(SetupDecodeError::InvalidState(format!(
                    "{} cannot appear in completed/skipped",
                    step.key()
                )));
            }
        }
        for (index, step) in self.completed.iter().enumerate() {
            if self.completed[index + 1..].contains(step) || self.skipped.contains(step) {
                return Err(SetupDecodeError::InvalidState(format!(
                    "duplicate or conflicting step {}",
                    step.key()
                )));
            }
        }
        for (index, step) in self.skipped.iter().enumerate() {
            if self.skipped[index + 1..].contains(step) {
                return Err(SetupDecodeError::InvalidState(format!(
                    "duplicate skipped step {}",
                    step.key()
                )));
            }
        }
        if self.status == SetupStatus::NotStarted
            && (self.current_step != SetupStep::Welcome
                || !self.completed.is_empty()
                || !self.skipped.is_empty()
                || self.attempts != 0
                || self.last_error.is_some())
        {
            return Err(SetupDecodeError::InvalidState(
                "not-started state contains progress".into(),
            ));
        }
        if self.status == SetupStatus::Failed
            && self
                .last_error
                .as_deref()
                .is_none_or(|message| message.trim().is_empty())
        {
            return Err(SetupDecodeError::InvalidState(
                "failed state has no error message".into(),
            ));
        }
        if self.status != SetupStatus::Failed && self.last_error.is_some() {
            return Err(SetupDecodeError::InvalidState(
                "only failed state may contain an error message".into(),
            ));
        }
        if self.status == SetupStatus::Complete {
            if self.current_step != SetupStep::Complete
                || actionable
                    .iter()
                    .any(|step| !self.completed.contains(step) && !self.skipped.contains(step))
            {
                return Err(SetupDecodeError::InvalidState(
                    "complete state does not account for every setup step".into(),
                ));
            }
        } else if self.current_step == SetupStep::Complete {
            return Err(SetupDecodeError::InvalidState(
                "only complete state may use the complete step".into(),
            ));
        }
        Ok(())
    }

    /// Serialize into a deterministic, versioned text representation.
    pub fn encode(&self) -> Result<String, SetupDecodeError> {
        self.validate()?;
        Ok(format!(
            "{PERSISTENCE_HEADER}\n\
             schema={}\n\
             status={}\n\
             current={}\n\
             completed={}\n\
             skipped={}\n\
             attempts={}\n\
             error={}\n",
            self.schema_version,
            self.status.key(),
            self.current_step.key(),
            encode_steps(&self.completed),
            encode_steps(&self.skipped),
            self.attempts,
            self.last_error
                .as_deref()
                .map(encode_hex)
                .unwrap_or_else(|| "-".into()),
        ))
    }

    /// Parse a previously persisted state.
    pub fn decode(contents: &str) -> Result<Self, SetupDecodeError> {
        let mut lines = contents.lines();
        if lines.next() != Some(PERSISTENCE_HEADER) {
            return Err(SetupDecodeError::InvalidHeader);
        }

        let mut schema = None;
        let mut status = None;
        let mut current = None;
        let mut completed = None;
        let mut skipped = None;
        let mut attempts = None;
        let mut error = None;
        for (line_index, line) in lines.enumerate() {
            let line_number = line_index + 2;
            if line.trim().is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(SetupDecodeError::MalformedLine { line: line_number });
            };
            match key {
                "schema" => set_once(&mut schema, value, "schema", line_number)?,
                "status" => set_once(&mut status, value, "status", line_number)?,
                "current" => set_once(&mut current, value, "current", line_number)?,
                "completed" => set_once(&mut completed, value, "completed", line_number)?,
                "skipped" => set_once(&mut skipped, value, "skipped", line_number)?,
                "attempts" => set_once(&mut attempts, value, "attempts", line_number)?,
                "error" => set_once(&mut error, value, "error", line_number)?,
                _ => {
                    // Unknown keys are ignored so a newer writer can add
                    // metadata without making an older UI unusable.
                }
            }
        }

        let schema_value = parse_field(schema, "schema")?;
        let schema = schema_value
            .parse::<u32>()
            .map_err(|_| SetupDecodeError::InvalidField {
                field: "schema",
                value: schema_value.clone(),
            })?;
        let status_key = parse_field(status, "status")?;
        let status = SetupStatus::from_key(&status_key).ok_or_else(|| SetupDecodeError::InvalidField {
            field: "status",
            value: status_key.clone(),
        })?;
        let current_key = parse_field(current, "current")?;
        let current_step =
            SetupStep::from_key(&current_key).ok_or_else(|| SetupDecodeError::InvalidField {
                field: "current",
                value: current_key.clone(),
            })?;
        let completed_value = parse_field(completed, "completed")?;
        let completed = parse_steps(&completed_value, "completed")?;
        let skipped_value = parse_field(skipped, "skipped")?;
        let skipped = parse_steps(&skipped_value, "skipped")?;
        let attempts_key = parse_field(attempts, "attempts")?;
        let attempts = attempts_key
            .parse::<u32>()
            .map_err(|_| SetupDecodeError::InvalidField {
                field: "attempts",
                value: attempts_key.clone(),
            })?;
        let error_key = parse_field(error, "error")?;
        let last_error = if error_key == "-" {
            None
        } else {
            Some(decode_hex(&error_key).map_err(|reason| SetupDecodeError::InvalidHex { reason })?)
        };

        let state = Self {
            schema_version: schema,
            status,
            current_step,
            completed,
            skipped,
            attempts,
            last_error,
        };
        state.validate()?;
        Ok(state)
    }

    /// Produce the data required for an atomic persistence write.
    pub fn persistence_plan(
        &self,
        path: impl Into<PathBuf>,
    ) -> Result<PersistencePlan, SetupPersistenceError> {
        let path = path.into();
        if path.as_os_str().is_empty() || path.file_name().is_none() {
            return Err(SetupPersistenceError::InvalidPath { path });
        }
        let contents = self.encode().map_err(SetupPersistenceError::InvalidState)?;
        let temporary_path = PathBuf::from(format!("{}.tmp", path.to_string_lossy()));
        Ok(PersistencePlan {
            path,
            temporary_path,
            contents,
        })
    }
}

/// Construct the conventional per-user state location.
pub fn default_state_path(home: &Path) -> PathBuf {
    home.join(".local")
        .join("state")
        .join("rouch")
        .join(SETUP_STATE_FILENAME)
}

/// A UI action understood by SetupState::apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupAction {
    /// Begin the first attempt.
    Start,
    /// Complete the current step.
    Complete,
    /// Skip the current optional step.
    Skip,
    /// Record a recoverable failure.
    Fail(String),
    /// Clear the failure and retry the same step.
    Recover,
}

/// A small state transition report for rendering and logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupTransition {
    /// Status before the action.
    pub previous_status: SetupStatus,
    /// Status after the action.
    pub status: SetupStatus,
    /// Step before the action.
    pub previous_step: SetupStep,
    /// Step after the action.
    pub step: SetupStep,
    /// False for idempotent start/recover calls.
    pub changed: bool,
}

impl SetupTransition {
    fn unchanged(state: &SetupState) -> Self {
        Self {
            previous_status: state.status,
            status: state.status,
            previous_step: state.current_step,
            step: state.current_step,
            changed: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SetupSnapshot {
    status: SetupStatus,
    step: SetupStep,
}

/// Errors from invalid onboarding actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupError {
    /// Setup has not started yet.
    NotStarted,
    /// A failed stage must be recovered before another action.
    RecoverBeforeAction { action: &'static str },
    /// Start was called while recovery is required.
    RecoverBeforeStarting,
    /// The terminal state cannot be changed.
    AlreadyComplete,
    /// The selected step is required.
    CannotSkip { step: SetupStep },
    /// A failure must include a useful explanation.
    EmptyFailure,
}

impl fmt::Display for SetupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotStarted => formatter.write_str("setup has not started"),
            Self::RecoverBeforeAction { action } => {
                write!(formatter, "recover the failed step before {action}")
            }
            Self::RecoverBeforeStarting => formatter.write_str("recover the failed setup before starting"),
            Self::AlreadyComplete => formatter.write_str("setup is already complete"),
            Self::CannotSkip { step } => write!(formatter, "setup step {step} cannot be skipped"),
            Self::EmptyFailure => formatter.write_str("setup failure message cannot be empty"),
        }
    }
}

impl std::error::Error for SetupError {}

/// Data for an atomic writer: write temporary_path, flush, then rename to
/// path. The writer belongs to the integration layer so this module remains
/// side-effect free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistencePlan {
    /// Final state path.
    pub path: PathBuf,
    /// Temporary path to write before an atomic rename.
    pub temporary_path: PathBuf,
    /// Exact serialized contents.
    pub contents: String,
}

/// Errors while preparing persistence data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupPersistenceError {
    /// The target path cannot represent a file.
    InvalidPath { path: PathBuf },
    /// State failed validation before serialization.
    InvalidState(SetupDecodeError),
}

impl fmt::Display for SetupPersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { path } => write!(formatter, "invalid setup state path: {path:?}"),
            Self::InvalidState(error) => write!(formatter, "invalid setup state: {error}"),
        }
    }
}

impl std::error::Error for SetupPersistenceError {}

/// Errors from decoding persisted setup state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupDecodeError {
    /// Header is absent or from another schema family.
    InvalidHeader,
    /// A line did not contain key=value.
    MalformedLine { line: usize },
    /// A key occurred more than once.
    DuplicateField { field: &'static str, line: usize },
    /// A required field was not present.
    MissingField { field: &'static str },
    /// A field could not be interpreted.
    InvalidField { field: &'static str, value: String },
    /// An error message was not valid hexadecimal UTF-8 data.
    InvalidHex { reason: String },
    /// Parsed values violate a state invariant.
    InvalidState(String),
}

impl fmt::Display for SetupDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHeader => formatter.write_str("invalid setup state header"),
            Self::MalformedLine { line } => write!(formatter, "malformed setup state line {line}"),
            Self::DuplicateField { field, line } => {
                write!(formatter, "duplicate setup field {field} on line {line}")
            }
            Self::MissingField { field } => write!(formatter, "missing setup field {field}"),
            Self::InvalidField { field, value } => {
                write!(formatter, "invalid setup field {field}={value:?}")
            }
            Self::InvalidHex { reason } => write!(formatter, "invalid setup error encoding: {reason}"),
            Self::InvalidState(reason) => write!(formatter, "invalid setup state: {reason}"),
        }
    }
}

impl std::error::Error for SetupDecodeError {}

fn push_unique(values: &mut Vec<SetupStep>, value: SetupStep) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn encode_steps(steps: &[SetupStep]) -> String {
    steps.iter().map(|step| step.key()).collect::<Vec<_>>().join(",")
}

fn parse_steps(value: &str, field: &'static str) -> Result<Vec<SetupStep>, SetupDecodeError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    value
        .split(',')
        .map(|key| {
            SetupStep::from_key(key).ok_or_else(|| SetupDecodeError::InvalidField {
                field,
                value: key.to_owned(),
            })
        })
        .collect()
}

fn encode_hex(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn decode_hex(value: &str) -> Result<String, String> {
    if !value.len().is_multiple_of(2) {
        return Err("odd number of hexadecimal digits".into());
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    let chars: Vec<u8> = value.as_bytes().to_vec();
    for pair in chars.chunks_exact(2) {
        let high = hex_value(pair[0]).ok_or_else(|| "non-hex digit".to_owned())?;
        let low = hex_value(pair[1]).ok_or_else(|| "non-hex digit".to_owned())?;
        bytes.push((high << 4) | low);
    }
    String::from_utf8(bytes).map_err(|_| "error text is not UTF-8".into())
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn set_once(
    target: &mut Option<String>,
    value: &str,
    field: &'static str,
    line: usize,
) -> Result<(), SetupDecodeError> {
    if target.is_some() {
        return Err(SetupDecodeError::DuplicateField { field, line });
    }
    *target = Some(value.to_owned());
    Ok(())
}

fn parse_field(field: Option<String>, name: &'static str) -> Result<String, SetupDecodeError> {
    field.ok_or(SetupDecodeError::MissingField { field: name })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finish_required_except_optional() -> SetupState {
        let mut state = SetupState::new();
        state.start().unwrap();
        for _ in 0..3 {
            state.complete_current().unwrap();
        }
        state.skip_current().unwrap();
        state.complete_current().unwrap();
        state.skip_current().unwrap();
        state.complete_current().unwrap();
        state
    }

    #[test]
    fn setup_advances_in_order_and_optional_steps_can_be_skipped() {
        let mut state = SetupState::new();
        assert_eq!(state.status, SetupStatus::NotStarted);
        state.start().unwrap();
        assert_eq!(state.attempts, 1);
        assert_eq!(state.current_step, SetupStep::Welcome);

        state.complete_current().unwrap();
        state.complete_current().unwrap();
        state.complete_current().unwrap();
        assert_eq!(state.current_step, SetupStep::Swapfile);
        state.skip_current().unwrap();
        assert_eq!(state.current_step, SetupStep::DesktopEntry);
        assert!(!state.is_complete());
    }

    #[test]
    fn failures_are_recoverable_without_losing_completed_steps() {
        let mut state = SetupState::new();
        state.start().unwrap();
        state.complete_current().unwrap();
        state.fail("cargo build failed").unwrap();

        assert_eq!(state.status, SetupStatus::Failed);
        assert_eq!(state.current_step, SetupStep::Dependencies);
        assert_eq!(state.completed, vec![SetupStep::Welcome]);
        state.recover().unwrap();
        assert_eq!(state.status, SetupStatus::InProgress);
        assert_eq!(state.last_error, None);
        assert_eq!(state.attempts, 2);
    }

    #[test]
    fn required_step_cannot_be_skipped() {
        let mut state = SetupState::new();
        state.start().unwrap();
        assert_eq!(
            state.skip_current(),
            Err(SetupError::CannotSkip {
                step: SetupStep::Welcome
            })
        );
    }

    #[test]
    fn persistence_round_trip_preserves_unicode_error() {
        let mut state = SetupState::new();
        state.start().unwrap();
        state.fail("Falha Vulkan — tente OpenGL").unwrap();
        let encoded = state.encode().unwrap();
        let decoded = SetupState::decode(&encoded).unwrap();

        assert_eq!(decoded, state);
        assert!(encoded.starts_with("rouch-setup-v1\n"));
    }

    #[test]
    fn complete_state_requires_every_stage_to_be_accounted_for() {
        let state = finish_required_except_optional();
        assert_eq!(state.status, SetupStatus::Complete);
        assert_eq!(state.current_step, SetupStep::Complete);
        assert!(state.completed.contains(&SetupStep::Welcome));
        assert!(state.skipped.contains(&SetupStep::Swapfile));
    }

    #[test]
    fn persistence_plan_is_atomic_writer_input() {
        let state = SetupState::new();
        let plan = state
            .persistence_plan(default_state_path(Path::new("/home/alice")))
            .unwrap();

        assert_eq!(
            plan.path,
            PathBuf::from("/home/alice/.local/state/rouch/setup.state")
        );
        assert_eq!(
            plan.temporary_path,
            PathBuf::from("/home/alice/.local/state/rouch/setup.state.tmp")
        );
        assert!(plan.contents.contains("status=not-started"));
    }

    #[test]
    fn unknown_future_keys_are_ignored_but_duplicate_known_keys_fail() {
        let state = SetupState::new();
        let encoded = state.encode().unwrap();
        let with_future_key = format!("{encoded}future=value\n");
        assert!(SetupState::decode(&with_future_key).is_ok());
        let duplicate = format!("{encoded}status=complete\n");
        assert!(matches!(
            SetupState::decode(&duplicate),
            Err(SetupDecodeError::DuplicateField { field: "status", .. })
        ));
    }
}
