//! Pure swapfile sizing, inspection, and command planning.
//!
//! The compositor never changes swap state implicitly. A platform adapter
//! reads /proc/swaps and the target file metadata, constructs a
//! SwapfileObservation, and asks this module for a plan. The plan contains
//! exact argv vectors; execution is available only through an injected
//! CommandExecutor. This keeps the policy testable and prevents a library
//! call from silently invoking a shell or deleting a user's file.

use std::{
    fmt,
    path::{Component, Path, PathBuf},
};

/// Number of bytes in a kibibyte.
pub const KIBIBYTE: u64 = 1024;
/// Number of bytes in a mebibyte.
pub const MEBIBYTE: u64 = 1024 * KIBIBYTE;
/// Number of bytes in a gibibyte.
pub const GIBIBYTE: u64 = 1024 * MEBIBYTE;

/// Smallest swapfile this policy will create.
pub const MIN_SWAPFILE_BYTES: u64 = 256 * MEBIBYTE;
/// Largest swapfile this policy will create without explicit policy changes.
pub const MAX_SWAPFILE_BYTES: u64 = 64 * GIBIBYTE;
/// Recommended upper bound avoids allocating enormous swapfiles on large RAM
/// machines while still giving memory pressure room for a desktop session.
pub const MAX_RECOMMENDED_SWAP_BYTES: u64 = 8 * GIBIBYTE;
/// Secure permissions required for a swapfile.
pub const SECURE_SWAPFILE_MODE: u32 = 0o600;
/// Moderate positive priority for the Rouch-managed swapfile.
pub const DEFAULT_SWAP_PRIORITY: i32 = 100;

/// Default location used by the installer and settings integration.
pub const DEFAULT_SWAPFILE_PATH: &str = "/var/lib/rouch/rouch.swap";

/// Filesystem information required to choose a safe swapfile creation path.
///
/// `Unknown` deliberately selects the portable zero-fill plan. A caller may
/// use `from_name` with a trusted filesystem probe; the planner never guesses
/// that an arbitrary filesystem accepts preallocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SwapFilesystem {
    Ext4,
    Xfs,
    Btrfs,
    #[default]
    Unknown,
    Other,
}

impl SwapFilesystem {
    pub fn from_name(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "ext4" => Self::Ext4,
            "xfs" => Self::Xfs,
            "btrfs" => Self::Btrfs,
            "" => Self::Unknown,
            _ => Self::Other,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Ext4 => "ext4",
            Self::Xfs => "xfs",
            Self::Btrfs => "btrfs",
            Self::Unknown => "unknown",
            Self::Other => "other",
        }
    }
}

/// How a new regular file receives its blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SwapFileAllocation {
    /// Write every byte from `/dev/zero`; this is the portable fallback for
    /// filesystems whose extent rules have not been identified.
    ZeroFill,
    /// Use filesystem preallocation where the probe says it is supported.
    Preallocate,
}

impl SwapFilesystem {
    const fn allocation(self) -> SwapFileAllocation {
        match self {
            Self::Ext4 | Self::Xfs => SwapFileAllocation::Preallocate,
            Self::Btrfs | Self::Unknown | Self::Other => SwapFileAllocation::ZeroFill,
        }
    }
}

/// Errors raised before a swap plan can be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapfileError {
    /// The requested path is empty.
    EmptyPath,
    /// A swapfile must have an absolute path so the command cannot be
    /// redirected by a changed working directory.
    RelativePath { path: PathBuf },
    /// The filesystem root or a directory path cannot be used as a file.
    UnsafePath { path: PathBuf },
    /// Size is outside the safe policy bounds.
    InvalidSize { size_bytes: u64 },
    /// Sizes are rounded to whole mebibytes to make command output stable.
    UnalignedSize { size_bytes: u64 },
    /// Swap priority must fit the Linux swapon range.
    InvalidPriority { priority: i32 },
}

impl fmt::Display for SwapfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPath => formatter.write_str("swapfile path cannot be empty"),
            Self::RelativePath { path } => {
                write!(formatter, "swapfile path must be absolute: {path:?}")
            }
            Self::UnsafePath { path } => write!(formatter, "unsafe swapfile path: {path:?}"),
            Self::InvalidSize { size_bytes } => {
                write!(
                    formatter,
                    "swapfile size is outside the safe range: {size_bytes} bytes"
                )
            }
            Self::UnalignedSize { size_bytes } => write!(
                formatter,
                "swapfile size must be aligned to 1 MiB: {size_bytes} bytes"
            ),
            Self::InvalidPriority { priority } => {
                write!(
                    formatter,
                    "swapfile priority is outside Linux's range: {priority}"
                )
            }
        }
    }
}

impl std::error::Error for SwapfileError {}

/// Validate a path before it is placed in an explicit command.
pub fn validate_swapfile_path(path: &Path) -> Result<(), SwapfileError> {
    if path.as_os_str().is_empty() {
        return Err(SwapfileError::EmptyPath);
    }
    if path.to_string_lossy().contains('\0') {
        return Err(SwapfileError::UnsafePath {
            path: path.to_path_buf(),
        });
    }
    // The policy targets Linux paths. Accept a leading slash even when the
    // pure tests run on Windows, where std::path::Path::is_absolute follows
    // Windows drive-letter rules instead of POSIX rules.
    let posix_absolute = path.to_string_lossy().starts_with('/');
    if !path.is_absolute() && !posix_absolute {
        return Err(SwapfileError::RelativePath {
            path: path.to_path_buf(),
        });
    }
    if path.parent().is_none() || path == Path::new("/") || path.file_name().is_none() {
        return Err(SwapfileError::UnsafePath {
            path: path.to_path_buf(),
        });
    }
    // Keep the path canonical at the planner boundary. The privileged
    // executor must still reject symlinks at the filesystem boundary, but
    // refusing `.` and `..` here prevents two spellings of the same target
    // from producing confusing plans and logs.
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(SwapfileError::UnsafePath {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

/// Validate an exact byte count accepted by the planner.
pub fn validate_swapfile_size(size_bytes: u64) -> Result<(), SwapfileError> {
    if !(MIN_SWAPFILE_BYTES..=MAX_SWAPFILE_BYTES).contains(&size_bytes) {
        return Err(SwapfileError::InvalidSize { size_bytes });
    }
    if !size_bytes.is_multiple_of(MEBIBYTE) {
        return Err(SwapfileError::UnalignedSize { size_bytes });
    }
    Ok(())
}

fn validate_priority(priority: i32) -> Result<(), SwapfileError> {
    // Linux accepts priorities from -1 to 32767. Rouch deliberately uses a
    // non-negative value so it never unexpectedly outranks administrator
    // configured emergency swap devices.
    if !(0..=32_767).contains(&priority) {
        return Err(SwapfileError::InvalidPriority { priority });
    }
    Ok(())
}

/// Calculate a conservative swapfile size from physical memory.
///
/// The policy recommends about half of physical memory, then clamps the
/// result to 1--8 GiB. It is intentionally a recommendation, not an
/// instruction to resize an existing swap device. A larger swap area does not
/// create RAM and can make a memory-starved desktop thrash for longer.
pub fn recommended_swap_size(memory_bytes: u64) -> Result<u64, SwapfileError> {
    if memory_bytes == 0 {
        return Err(SwapfileError::InvalidSize { size_bytes: 0 });
    }

    let rounded_memory = memory_bytes / MEBIBYTE * MEBIBYTE;
    let rounded_memory = rounded_memory.max(MEBIBYTE);
    let target = (rounded_memory / 2).clamp(GIBIBYTE, MAX_RECOMMENDED_SWAP_BYTES);
    validate_swapfile_size(target)?;
    Ok(target)
}

/// A validated swapfile request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapfileRequest {
    /// Absolute target path.
    pub path: PathBuf,
    /// Desired capacity in bytes.
    pub size_bytes: u64,
    /// swapon --priority value.
    pub priority: i32,
}

impl SwapfileRequest {
    /// Build a request using the default priority.
    pub fn new(path: impl Into<PathBuf>, size_bytes: u64) -> Result<Self, SwapfileError> {
        Self::with_priority(path, size_bytes, DEFAULT_SWAP_PRIORITY)
    }

    /// Build a request with an explicit priority.
    pub fn with_priority(
        path: impl Into<PathBuf>,
        size_bytes: u64,
        priority: i32,
    ) -> Result<Self, SwapfileError> {
        let path = path.into();
        validate_swapfile_path(&path)?;
        validate_swapfile_size(size_bytes)?;
        validate_priority(priority)?;
        Ok(Self {
            path,
            size_bytes,
            priority,
        })
    }

    /// Build the default request from physical memory.
    pub fn from_memory(path: impl Into<PathBuf>, memory_bytes: u64) -> Result<Self, SwapfileError> {
        Self::new(path, recommended_swap_size(memory_bytes)?)
    }
}

/// One active entry parsed from /proc/swaps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSwap {
    /// Filename or block-device path reported by Linux.
    pub path: PathBuf,
    /// Size reported by Linux in bytes.
    pub size_bytes: u64,
    /// Current kernel priority.
    pub priority: i32,
}

/// Parse the stable, whitespace-delimited /proc/swaps format.
pub fn parse_proc_swaps(contents: &str) -> Result<Vec<ActiveSwap>, SwapParseError> {
    let mut entries = Vec::new();
    for (line_number, line) in contents.lines().enumerate() {
        let line_number = line_number + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("Filename") {
            continue;
        }
        let fields: Vec<&str> = trimmed.split_whitespace().collect();
        if fields.len() < 5 {
            return Err(SwapParseError::MalformedLine {
                line: line_number,
                contents: line.to_owned(),
            });
        }
        let size_kib = fields[2]
            .parse::<u64>()
            .map_err(|_| SwapParseError::InvalidNumber {
                line: line_number,
                field: "size",
                value: fields[2].to_owned(),
            })?;
        let size_bytes = size_kib
            .checked_mul(KIBIBYTE)
            .ok_or(SwapParseError::Overflow { line: line_number })?;
        let priority = fields[4]
            .parse::<i32>()
            .map_err(|_| SwapParseError::InvalidNumber {
                line: line_number,
                field: "priority",
                value: fields[4].to_owned(),
            })?;
        entries.push(ActiveSwap {
            path: PathBuf::from(fields[0]),
            size_bytes,
            priority,
        });
    }
    Ok(entries)
}

/// Errors from parsing /proc/swaps supplied by a platform adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapParseError {
    /// A data line did not contain the expected fields.
    MalformedLine { line: usize, contents: String },
    /// One numeric field was not parseable.
    InvalidNumber {
        line: usize,
        field: &'static str,
        value: String,
    },
    /// A KiB-to-byte conversion overflowed.
    Overflow { line: usize },
}

impl fmt::Display for SwapParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedLine { line, .. } => {
                write!(formatter, "malformed /proc/swaps line {line}")
            }
            Self::InvalidNumber { line, field, value } => {
                write!(formatter, "invalid {field} {value:?} on /proc/swaps line {line}")
            }
            Self::Overflow { line } => write!(formatter, "size overflow on /proc/swaps line {line}"),
        }
    }
}

impl std::error::Error for SwapParseError {}

/// Metadata collected for the requested path without performing any action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapfileMetadata {
    /// Whether the path exists.
    pub exists: bool,
    /// Whether the path is a regular file.
    pub is_regular: bool,
    /// Whether the target path itself is a symlink.
    pub is_symlink: bool,
    /// File size in bytes when known.
    pub size_bytes: Option<u64>,
    /// Unix permission bits when known.
    pub mode: Option<u32>,
    /// Owner uid when known. None means the adapter could not inspect it.
    pub owner_uid: Option<u32>,
    /// Whether the file carries a valid swap signature.
    pub is_swap: bool,
    /// Btrfs NOCOW state when the platform adapter can inspect it.
    /// `None` is intentionally not treated as safe for an existing Btrfs file.
    pub is_nocow: Option<bool>,
}

impl SwapfileMetadata {
    /// Metadata representing an absent target.
    pub const fn absent() -> Self {
        Self {
            exists: false,
            is_regular: false,
            is_symlink: false,
            size_bytes: None,
            mode: None,
            owner_uid: None,
            is_swap: false,
            is_nocow: None,
        }
    }

    /// Construct metadata for a normal existing swapfile.
    pub const fn existing(size_bytes: u64, mode: u32, owner_uid: Option<u32>, is_swap: bool) -> Self {
        Self {
            exists: true,
            is_regular: true,
            is_symlink: false,
            size_bytes: Some(size_bytes),
            mode: Some(mode),
            owner_uid,
            is_swap,
            is_nocow: None,
        }
    }

    /// Construct metadata for a file whose Btrfs NOCOW state was inspected.
    pub const fn existing_with_nocow(
        size_bytes: u64,
        mode: u32,
        owner_uid: Option<u32>,
        is_swap: bool,
        is_nocow: bool,
    ) -> Self {
        let mut metadata = Self::existing(size_bytes, mode, owner_uid, is_swap);
        metadata.is_nocow = Some(is_nocow);
        metadata
    }
}

/// All observations needed to create an idempotent plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapfileObservation {
    /// Target-file metadata.
    pub target: SwapfileMetadata,
    /// Filesystem hosting the target path. Unknown selects the portable plan.
    pub filesystem: SwapFilesystem,
    /// Active swap devices/files reported by the kernel.
    pub active_swaps: Vec<ActiveSwap>,
}

impl SwapfileObservation {
    /// Construct an observation for an absent target and no active entries.
    pub fn absent() -> Self {
        Self {
            target: SwapfileMetadata::absent(),
            filesystem: SwapFilesystem::Unknown,
            active_swaps: Vec::new(),
        }
    }

    pub fn absent_on(filesystem: SwapFilesystem) -> Self {
        Self {
            target: SwapfileMetadata::absent(),
            filesystem,
            active_swaps: Vec::new(),
        }
    }

    /// Whether the requested path is active in the supplied kernel snapshot.
    pub fn target_is_active(&self, path: &Path) -> bool {
        self.active_swaps.iter().any(|entry| entry.path == path)
    }
}

/// High-level result of planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapPlanStatus {
    /// Nothing needs to be changed.
    Noop,
    /// All actions are non-destructive and may be executed.
    Ready,
    /// At least one action must be explicitly confirmed by the user.
    NeedsConfirmation,
    /// The adapter must resolve a safety issue manually.
    Blocked,
}

/// An exact command argument vector, deliberately not a shell string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Executable name.
    pub program: String,
    /// Arguments in the order passed to the executable.
    pub args: Vec<String>,
    /// Whether this command can overwrite or disable existing swap state.
    pub destructive: bool,
    /// Human-readable reason for logs and confirmation UI.
    pub description: String,
}

impl CommandSpec {
    fn new(
        program: impl Into<String>,
        args: Vec<String>,
        destructive: bool,
        description: impl Into<String>,
    ) -> Self {
        Self {
            program: program.into(),
            args,
            destructive,
            description: description.into(),
        }
    }
}

/// One safe or explicitly confirmable operation in a swap plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapAction {
    /// Create a swapfile with an explicitly selected allocation strategy.
    CreateFile {
        path: PathBuf,
        size_bytes: u64,
        allocation: SwapFileAllocation,
    },
    /// Create an empty file before setting Btrfs NOCOW. This ordering is
    /// required because changing NOCOW after extents exist is not enough.
    CreateEmptyFile { path: PathBuf },
    /// Mark an empty Btrfs file as NOCOW before allocating its extents.
    DisableCopyOnWrite { path: PathBuf },
    /// Ensure the parent directory exists.
    EnsureParent { path: PathBuf },
    /// Set private permissions on the target.
    SetPermissions { path: PathBuf, mode: u32 },
    /// Write the Linux swap signature.
    FormatSwap { path: PathBuf },
    /// Enable the target with an explicit priority.
    Enable { path: PathBuf, priority: i32 },
    /// No command is necessary.
    Noop { reason: String },
    /// The plan refuses to guess at a potentially destructive repair.
    ManualIntervention { path: PathBuf, reason: String },
}

impl SwapAction {
    /// Convert an action into its exact command, if it has one.
    pub fn command(&self) -> Option<CommandSpec> {
        match self {
            Self::CreateFile {
                path,
                size_bytes,
                allocation,
            } => match allocation {
                SwapFileAllocation::Preallocate => Some(CommandSpec::new(
                    "fallocate",
                    vec![
                        "-l".into(),
                        size_bytes.to_string(),
                        path.to_string_lossy().into_owned(),
                    ],
                    false,
                    format!("preallocate {size_bytes} bytes for the Rouch swapfile"),
                )),
                SwapFileAllocation::ZeroFill => Some(CommandSpec::new(
                    "dd",
                    vec![
                        "if=/dev/zero".into(),
                        format!("of={}", path.to_string_lossy()),
                        "bs=1M".into(),
                        format!("count={}", size_bytes / MEBIBYTE),
                        "conv=excl".into(),
                        "status=none".into(),
                    ],
                    false,
                    format!("zero-fill {size_bytes} bytes for the Rouch swapfile"),
                )),
            },
            Self::CreateEmptyFile { path } => Some(CommandSpec::new(
                "dd",
                vec![
                    "if=/dev/zero".into(),
                    format!("of={}", path.to_string_lossy()),
                    "bs=1".into(),
                    "count=0".into(),
                    "conv=excl".into(),
                    "status=none".into(),
                ],
                false,
                "create an empty Btrfs swapfile before disabling copy-on-write",
            )),
            Self::DisableCopyOnWrite { path } => Some(CommandSpec::new(
                "chattr",
                vec!["+C".into(), path.to_string_lossy().into_owned()],
                false,
                "disable copy-on-write on the empty Btrfs swapfile",
            )),
            Self::EnsureParent { path } => Some(CommandSpec::new(
                "install",
                vec![
                    "-d".into(),
                    "-m".into(),
                    "0755".into(),
                    path.to_string_lossy().into_owned(),
                ],
                false,
                "create the swapfile parent directory",
            )),
            Self::SetPermissions { path, mode } => Some(CommandSpec::new(
                "chmod",
                vec![format!("{mode:o}"), path.to_string_lossy().into_owned()],
                false,
                "restrict swapfile permissions to the owner",
            )),
            Self::FormatSwap { path } => Some(CommandSpec::new(
                "mkswap",
                vec![path.to_string_lossy().into_owned()],
                true,
                "write a swap signature to the target file",
            )),
            Self::Enable { path, priority } => Some(CommandSpec::new(
                "swapon",
                vec![
                    "--priority".into(),
                    priority.to_string(),
                    path.to_string_lossy().into_owned(),
                ],
                false,
                "activate the Rouch swapfile",
            )),
            Self::Noop { .. } | Self::ManualIntervention { .. } => None,
        }
    }

    /// Whether a user confirmation is required before this action runs.
    pub const fn requires_confirmation(&self) -> bool {
        matches!(self, Self::FormatSwap { .. })
    }
}

/// Complete action plan produced from a request and an observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapfilePlan {
    /// Target request used to produce this plan.
    pub request: SwapfileRequest,
    /// Overall safety state.
    pub status: SwapPlanStatus,
    /// Ordered actions; dependencies always precede activation.
    pub actions: Vec<SwapAction>,
    /// Explanations useful in settings and installer UI.
    pub notes: Vec<String>,
}

impl SwapfilePlan {
    /// Return only executable command specs, preserving action order.
    pub fn commands(&self) -> Vec<CommandSpec> {
        self.actions.iter().filter_map(SwapAction::command).collect()
    }

    /// Whether at least one command must be confirmed.
    pub fn requires_confirmation(&self) -> bool {
        self.actions.iter().any(SwapAction::requires_confirmation)
    }

    /// Whether this plan has a manual safety block.
    pub fn is_blocked(&self) -> bool {
        self.status == SwapPlanStatus::Blocked
    }
}

/// Build an idempotent plan without touching the filesystem or kernel.
pub fn plan_swapfile(request: SwapfileRequest, observation: SwapfileObservation) -> SwapfilePlan {
    let path = request.path.clone();
    let mut actions = Vec::new();
    let mut notes = Vec::new();

    if observation.target.exists {
        if observation.target.is_symlink {
            actions.push(SwapAction::ManualIntervention {
                path: path.clone(),
                reason: "the target path is a symlink; refusing to follow it".into(),
            });
            return blocked_plan(request, actions, notes);
        }
        if !observation.target.is_regular {
            actions.push(SwapAction::ManualIntervention {
                path: path.clone(),
                reason: "the target exists but is not a regular file".into(),
            });
            return blocked_plan(request, actions, notes);
        }
        if observation.filesystem == SwapFilesystem::Btrfs && observation.target.is_nocow != Some(true) {
            actions.push(SwapAction::ManualIntervention {
                path: path.clone(),
                reason: "an existing Btrfs target is not verified as NOCOW; recreate or inspect it manually"
                    .into(),
            });
            notes.push(
                "Btrfs NOCOW must be set before extents are allocated; automatic repair could invalidate active swap".into(),
            );
            return blocked_plan(request, actions, notes);
        }
        if observation.target.owner_uid.is_some_and(|uid| uid != 0) {
            actions.push(SwapAction::ManualIntervention {
                path: path.clone(),
                reason: "the target is not owned by root".into(),
            });
            return blocked_plan(request, actions, notes);
        }
        if observation.target.size_bytes != Some(request.size_bytes) {
            actions.push(SwapAction::ManualIntervention {
                path: path.clone(),
                reason: format!(
                    "existing size {:?} differs from requested {} bytes; refusing automatic resize",
                    observation.target.size_bytes, request.size_bytes
                ),
            });
            notes.push(
                "resize or replace the existing swapfile explicitly after checking active swap state".into(),
            );
            return blocked_plan(request, actions, notes);
        }
        if observation.target.mode != Some(SECURE_SWAPFILE_MODE) {
            actions.push(SwapAction::SetPermissions {
                path: path.clone(),
                mode: SECURE_SWAPFILE_MODE,
            });
        }
        if !observation.target.is_swap {
            actions.push(SwapAction::FormatSwap { path: path.clone() });
        }
        if observation.target_is_active(&path) {
            if actions.is_empty() {
                actions.push(SwapAction::Noop {
                    reason: "the requested swapfile is already active with the requested size".into(),
                });
            } else {
                notes.push("the target is active; permission/signature repair must be reviewed".into());
            }
        } else {
            actions.push(SwapAction::Enable {
                path,
                priority: request.priority,
            });
        }
    } else {
        let parent = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/"));
        actions.push(SwapAction::EnsureParent { path: parent });
        if observation.filesystem == SwapFilesystem::Btrfs {
            // Btrfs requires an empty file to be marked NOCOW before
            // allocation. This mirrors the documented fallback sequence and
            // works on systems without the newer mkswapfile helper.
            actions.push(SwapAction::CreateEmptyFile { path: path.clone() });
            actions.push(SwapAction::DisableCopyOnWrite { path: path.clone() });
            actions.push(SwapAction::CreateFile {
                path: path.clone(),
                size_bytes: request.size_bytes,
                allocation: SwapFileAllocation::Preallocate,
            });
        } else {
            actions.push(SwapAction::CreateFile {
                path: path.clone(),
                size_bytes: request.size_bytes,
                allocation: observation.filesystem.allocation(),
            });
            if observation.filesystem == SwapFilesystem::Unknown {
                notes.push("filesystem was not identified; using portable zero-fill allocation".into());
            }
        }
        actions.push(SwapAction::SetPermissions {
            path: path.clone(),
            mode: SECURE_SWAPFILE_MODE,
        });
        actions.push(SwapAction::FormatSwap { path: path.clone() });
        actions.push(SwapAction::Enable {
            path,
            priority: request.priority,
        });
        notes.push("new swap signatures require explicit confirmation before execution".into());
    }

    let status = if actions
        .iter()
        .all(|action| matches!(action, SwapAction::Noop { .. }))
    {
        SwapPlanStatus::Noop
    } else if actions.iter().any(SwapAction::requires_confirmation) {
        SwapPlanStatus::NeedsConfirmation
    } else {
        SwapPlanStatus::Ready
    };
    SwapfilePlan {
        request,
        status,
        actions,
        notes,
    }
}

fn blocked_plan(request: SwapfileRequest, actions: Vec<SwapAction>, notes: Vec<String>) -> SwapfilePlan {
    SwapfilePlan {
        request,
        status: SwapPlanStatus::Blocked,
        actions,
        notes,
    }
}

/// Whether the executor is allowed to run confirmable commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Inspect/preview only; no executor call is made.
    DryRun,
    /// The caller has obtained explicit user confirmation.
    Confirmed,
}

/// Minimal execution seam. The real integration can wrap
/// std::process::Command and add sudo; tests can record argv without touching
/// the host.
pub trait CommandExecutor {
    /// Execute one exact command specification.
    fn execute(&mut self, command: &CommandSpec) -> Result<(), String>;
}

/// Result of an injected execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionReport {
    /// Number of commands passed to the executor.
    pub executed: usize,
}

/// Errors raised before or during explicit command execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapExecutionError {
    /// The plan contains a manual safety block.
    Blocked,
    /// At least one command needs confirmation and mode was DryRun.
    ConfirmationRequired,
    /// The injected executor failed.
    CommandFailed { command: CommandSpec, message: String },
}

impl fmt::Display for SwapExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blocked => formatter.write_str("swap plan is blocked pending manual intervention"),
            Self::ConfirmationRequired => formatter.write_str("swap plan requires explicit confirmation"),
            Self::CommandFailed { command, message } => {
                write!(formatter, "{} failed: {message}", command.program)
            }
        }
    }
}

impl std::error::Error for SwapExecutionError {}

/// Execute only the commands represented by a plan through an injected
/// executor. This function never invokes a shell and never invents a
/// destructive command.
pub fn execute_plan<E: CommandExecutor>(
    plan: &SwapfilePlan,
    executor: &mut E,
    mode: ExecutionMode,
) -> Result<ExecutionReport, SwapExecutionError> {
    if plan.is_blocked() {
        return Err(SwapExecutionError::Blocked);
    }
    if plan.requires_confirmation() && mode != ExecutionMode::Confirmed {
        return Err(SwapExecutionError::ConfirmationRequired);
    }
    if mode == ExecutionMode::DryRun {
        return Ok(ExecutionReport { executed: 0 });
    }

    let mut executed = 0;
    for command in plan.commands() {
        if let Err(message) = executor.execute(&command) {
            return Err(SwapExecutionError::CommandFailed { command, message });
        }
        executed += 1;
    }
    Ok(ExecutionReport { executed })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommendation_is_bounded_and_aligned() {
        assert_eq!(recommended_swap_size(2 * GIBIBYTE).unwrap(), GIBIBYTE);
        assert_eq!(recommended_swap_size(4 * GIBIBYTE).unwrap(), 2 * GIBIBYTE);
        assert_eq!(recommended_swap_size(16 * GIBIBYTE).unwrap(), 8 * GIBIBYTE);
        assert_eq!(recommended_swap_size(128 * GIBIBYTE).unwrap(), 8 * GIBIBYTE);
        assert_eq!(
            recommended_swap_size(0),
            Err(SwapfileError::InvalidSize { size_bytes: 0 })
        );
    }

    #[test]
    fn proc_swaps_parser_converts_kib_to_bytes() {
        let input = "Filename\t\tType\tSize\tUsed\tPriority\n/swapfile file 262144 0 -2\n";
        let entries = parse_proc_swaps(input).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("/swapfile"));
        assert_eq!(entries[0].size_bytes, 256 * MEBIBYTE);
        assert_eq!(entries[0].priority, -2);
    }

    #[test]
    fn absent_target_produces_ordered_confirmable_plan() {
        let request = SwapfileRequest::new(DEFAULT_SWAPFILE_PATH, GIBIBYTE).unwrap();
        let plan = plan_swapfile(request, SwapfileObservation::absent());

        assert_eq!(plan.status, SwapPlanStatus::NeedsConfirmation);
        assert_eq!(plan.commands()[0].program, "install");
        assert_eq!(plan.commands()[1].program, "dd");
        assert_eq!(plan.commands()[2].args, vec!["600", DEFAULT_SWAPFILE_PATH]);
        assert!(plan.commands().iter().any(|command| command.destructive));
    }

    #[test]
    fn known_ext4_uses_preallocation() {
        let request = SwapfileRequest::new(DEFAULT_SWAPFILE_PATH, GIBIBYTE).unwrap();
        let plan = plan_swapfile(request, SwapfileObservation::absent_on(SwapFilesystem::Ext4));
        assert_eq!(plan.commands()[1].program, "fallocate");
    }

    #[test]
    fn btrfs_plan_sets_nocow_before_allocating() {
        let request = SwapfileRequest::new(DEFAULT_SWAPFILE_PATH, GIBIBYTE).unwrap();
        let plan = plan_swapfile(request, SwapfileObservation::absent_on(SwapFilesystem::Btrfs));
        let commands = plan.commands();
        assert_eq!(commands[1].program, "dd");
        assert_eq!(commands[2].program, "chattr");
        assert_eq!(commands[2].args[0], "+C");
        assert_eq!(commands[3].program, "fallocate");
    }

    #[test]
    fn existing_btrfs_target_without_verified_nocow_is_blocked() {
        let request = SwapfileRequest::new(DEFAULT_SWAPFILE_PATH, GIBIBYTE).unwrap();
        let observation = SwapfileObservation {
            target: SwapfileMetadata::existing(GIBIBYTE, SECURE_SWAPFILE_MODE, Some(0), true),
            filesystem: SwapFilesystem::Btrfs,
            active_swaps: Vec::new(),
        };
        let plan = plan_swapfile(request, observation);
        assert_eq!(plan.status, SwapPlanStatus::Blocked);
        assert!(matches!(plan.actions[0], SwapAction::ManualIntervention { .. }));
    }

    #[test]
    fn path_validation_rejects_ambiguous_components_and_nul() {
        assert!(matches!(
            validate_swapfile_path(Path::new("/var/lib/rouch/../swap")),
            Err(SwapfileError::UnsafePath { .. })
        ));
        assert!(matches!(
            validate_swapfile_path(Path::new("/var/lib/rouch/\0swap")),
            Err(SwapfileError::UnsafePath { .. })
        ));
    }

    #[test]
    fn symlink_target_is_never_repaired_automatically() {
        let request = SwapfileRequest::new(DEFAULT_SWAPFILE_PATH, GIBIBYTE).unwrap();
        let mut target = SwapfileMetadata::existing(GIBIBYTE, SECURE_SWAPFILE_MODE, Some(0), true);
        target.is_symlink = true;
        let observation = SwapfileObservation {
            target,
            filesystem: SwapFilesystem::Ext4,
            active_swaps: Vec::new(),
        };
        let plan = plan_swapfile(request, observation);
        assert_eq!(plan.status, SwapPlanStatus::Blocked);
    }

    #[test]
    fn active_matching_target_is_idempotent() {
        let request = SwapfileRequest::new("/var/lib/rouch/rouch.swap", GIBIBYTE).unwrap();
        let observation = SwapfileObservation {
            target: SwapfileMetadata::existing(GIBIBYTE, SECURE_SWAPFILE_MODE, Some(0), true),
            filesystem: SwapFilesystem::Ext4,
            active_swaps: vec![ActiveSwap {
                path: PathBuf::from("/var/lib/rouch/rouch.swap"),
                size_bytes: GIBIBYTE,
                priority: DEFAULT_SWAP_PRIORITY,
            }],
        };
        let plan = plan_swapfile(request, observation);

        assert_eq!(plan.status, SwapPlanStatus::Noop);
        assert_eq!(plan.commands().len(), 0);
    }

    #[test]
    fn mismatched_existing_file_is_blocked_instead_of_resized() {
        let request = SwapfileRequest::new("/var/lib/rouch/rouch.swap", GIBIBYTE).unwrap();
        let observation = SwapfileObservation {
            target: SwapfileMetadata::existing(2 * GIBIBYTE, SECURE_SWAPFILE_MODE, Some(0), true),
            filesystem: SwapFilesystem::Ext4,
            active_swaps: Vec::new(),
        };
        let plan = plan_swapfile(request, observation);

        assert_eq!(plan.status, SwapPlanStatus::Blocked);
        assert!(matches!(plan.actions[0], SwapAction::ManualIntervention { .. }));
    }

    #[derive(Default)]
    struct Recorder {
        commands: Vec<CommandSpec>,
    }

    impl CommandExecutor for Recorder {
        fn execute(&mut self, command: &CommandSpec) -> Result<(), String> {
            self.commands.push(command.clone());
            Ok(())
        }
    }

    #[test]
    fn execution_requires_confirmation_and_uses_explicit_argv() {
        let request = SwapfileRequest::new(DEFAULT_SWAPFILE_PATH, GIBIBYTE).unwrap();
        let plan = plan_swapfile(request, SwapfileObservation::absent());
        let mut recorder = Recorder::default();

        assert_eq!(
            execute_plan(&plan, &mut recorder, ExecutionMode::DryRun),
            Err(SwapExecutionError::ConfirmationRequired)
        );
        let report = execute_plan(&plan, &mut recorder, ExecutionMode::Confirmed).unwrap();
        assert_eq!(report.executed, 5);
        assert_eq!(recorder.commands[1].args[0], "if=/dev/zero");
    }
}
