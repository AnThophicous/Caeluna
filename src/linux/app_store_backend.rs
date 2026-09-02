//! Linux Flatpak/Flathub backend for the native application gallery.
//!
//! All process calls use [`std::process::Command`] with an argument vector;
//! no shell is involved.  The backend keeps the last successful catalogue on
//! disk and can also be seeded with trusted local entries by the integration
//! layer.  If Flatpak is absent, the result is explicitly `Offline` instead
//! of presenting made-up server data.

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::app_store::{CatalogSource, CatalogState, FlatpakApp, FlatpakMetadata};

/// The cache file format version.  It is intentionally private to Rouch so a
/// future schema can be introduced without silently misreading old records.
const CACHE_HEADER: &str = "# ROUCH_FLATPAK_CACHE 1";

/// Columns accepted by the common Flatpak CLI versions used by the backend.
/// Optional AppStream fields are still parsed when a test or a newer wrapper
/// supplies them, but the live query only asks for widely supported columns.
const REMOTE_COLUMNS: &str = "application,name,summary,version,branch,arch,origin";
const INSTALLED_COLUMNS: &str = "application,name,version,branch,arch,origin";

/// Output from one safely executed external command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// Whether the child exited successfully.
    pub success: bool,
    /// Numeric exit code, when the process returned one.
    pub exit_code: Option<i32>,
    /// Standard output decoded lossily as UTF-8.
    pub stdout: String,
    /// Standard error decoded lossily as UTF-8.
    pub stderr: String,
}

impl CommandOutput {
    /// Build a successful fake result for backend tests or an embedding
    /// integration.
    pub fn success(stdout: impl Into<String>) -> Self {
        Self {
            success: true,
            exit_code: Some(0),
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    /// Build a failed fake result for backend tests or an embedding
    /// integration.
    pub fn failure(exit_code: Option<i32>, stderr: impl Into<String>) -> Self {
        Self {
            success: false,
            exit_code,
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }
}

/// Small seam around process execution.  Production uses
/// [`SystemCommandRunner`]; tests can inject a deterministic implementation
/// without touching the host's Flatpak installation.
pub trait FlatpakCommandRunner {
    /// Run `program` with already separated arguments.
    fn run(&self, program: &str, args: &[String]) -> io::Result<CommandOutput>;
}

/// The real Linux process runner.  It never invokes a shell.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemCommandRunner;

impl FlatpakCommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[String]) -> io::Result<CommandOutput> {
        let output = Command::new(program).args(args).output()?;
        Ok(CommandOutput {
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// A persisted catalogue retained between sessions.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CatalogCache {
    entries: Vec<FlatpakApp>,
    saved_at: Option<u64>,
}

impl CatalogCache {
    /// Build an in-memory cache from trusted entries.
    pub fn from_entries(entries: Vec<FlatpakApp>) -> Self {
        Self {
            entries,
            saved_at: None,
        }
    }

    /// The cached entries, in their last observed order.
    pub fn entries(&self) -> &[FlatpakApp] {
        &self.entries
    }

    /// Whether no cached entries are available.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Seconds since the Unix epoch when this cache was written in memory or
    /// loaded from disk.
    pub fn saved_at(&self) -> Option<u64> {
        self.saved_at
    }

    /// Replace entries and stamp the cache with the current wall clock.
    pub fn replace(&mut self, entries: Vec<FlatpakApp>) {
        self.entries = entries;
        self.saved_at = Some(unix_seconds());
    }

    /// Load the cache file. Malformed records are ignored individually so a
    /// single truncated line cannot hide the rest of an otherwise useful
    /// offline catalogue.
    pub fn load(path: impl AsRef<Path>) -> io::Result<Self> {
        let body = fs::read_to_string(path)?;
        let mut lines = body.lines();
        if lines.next() != Some(CACHE_HEADER) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported Rouch Flatpak cache header",
            ));
        }

        let mut cache = Self::default();
        for line in lines {
            if let Some(value) = line.strip_prefix("saved_at\t") {
                cache.saved_at = value.parse::<u64>().ok();
                continue;
            }
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(app) = decode_cache_app(line) {
                cache.entries.push(app);
            }
        }
        Ok(cache)
    }

    /// Persist the cache, creating its parent directory when needed.
    pub fn save(&self, path: impl AsRef<Path>) -> io::Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }

        let mut body = String::new();
        body.push_str(CACHE_HEADER);
        body.push('\n');
        body.push_str("saved_at\t");
        body.push_str(&self.saved_at.unwrap_or_else(unix_seconds).to_string());
        body.push('\n');
        for app in &self.entries {
            body.push_str(&encode_cache_app(app));
            body.push('\n');
        }
        fs::write(path, body)
    }
}

/// Compute the default per-user cache location without assuming that a home
/// directory exists. `None` means the caller should use memory only.
pub fn default_cache_path() -> Option<PathBuf> {
    if let Some(cache_home) = non_empty_env("XDG_CACHE_HOME") {
        return Some(PathBuf::from(cache_home).join("rouch").join("app-store.tsv"));
    }
    non_empty_env("HOME").map(|home| PathBuf::from(home).join(".cache/rouch/app-store.tsv"))
}

/// A complete catalogue response for the pure app-store model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSnapshot {
    pub apps: Vec<FlatpakApp>,
    pub state: CatalogState,
    pub source: CatalogSource,
    /// A diagnostic suitable for a status row; it is not required for the
    /// renderer to decide which state to draw.
    pub message: Option<String>,
}

impl CatalogSnapshot {
    fn new(
        apps: Vec<FlatpakApp>,
        state: CatalogState,
        source: CatalogSource,
        message: impl Into<Option<String>>,
    ) -> Self {
        Self {
            apps,
            state,
            source,
            message: message.into(),
        }
    }
}

/// Kind of a mutating Flatpak operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    Install,
    Update,
    Remove,
}

/// Explicit result status for a mutating operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStatus {
    /// The command returned exit code zero.
    Succeeded,
    /// Flatpak could not be executed at all.
    Unavailable,
    /// Flatpak ran and rejected or failed the operation.
    Failed,
    /// Input validation rejected the operation before a process was started.
    Rejected,
}

impl OperationStatus {
    /// Whether the requested change was completed.
    pub fn is_success(self) -> bool {
        self == Self::Succeeded
    }

    /// Whether the host lacks the Flatpak executable.
    pub fn is_unavailable(self) -> bool {
        self == Self::Unavailable
    }
}

/// Detailed, renderer-independent operation report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationReport {
    pub operation: OperationKind,
    pub app_id: Option<String>,
    pub status: OperationStatus,
    pub exit_code: Option<i32>,
    pub message: String,
    pub stdout: String,
    pub stderr: String,
    /// The exact argv (including the program name) sent to the runner.
    /// Keeping this visible makes auditing and integration logging simple.
    pub command: Vec<String>,
}

impl OperationReport {
    /// Whether the operation completed successfully.
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }

    /// Whether Flatpak was unavailable on the host.
    pub fn is_unavailable(&self) -> bool {
        self.status.is_unavailable()
    }
}

/// Backend state and Flatpak command surface.
pub struct FlatpakBackend<R = SystemCommandRunner> {
    runner: R,
    remote: String,
    cache: CatalogCache,
    cache_path: Option<PathBuf>,
    local_fallback: Vec<FlatpakApp>,
}

impl FlatpakBackend<SystemCommandRunner> {
    /// Use the real `flatpak` executable and the user's default cache path.
    pub fn new() -> Self {
        Self::with_runner(SystemCommandRunner)
    }

    /// Use the real runner and an explicit cache path, useful for profile
    /// directories or integration tests.
    pub fn with_cache_path(path: impl Into<PathBuf>) -> Self {
        Self::with_runner_and_cache_path(SystemCommandRunner, Some(path.into()))
    }
}

impl Default for FlatpakBackend<SystemCommandRunner> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: FlatpakCommandRunner> FlatpakBackend<R> {
    /// Construct a backend around an injected command runner.
    pub fn with_runner(runner: R) -> Self {
        Self::with_runner_and_cache_path(runner, default_cache_path())
    }

    /// Construct a backend with an injected runner and cache path.
    pub fn with_runner_and_cache_path(runner: R, cache_path: Option<PathBuf>) -> Self {
        let cache = cache_path
            .as_deref()
            .and_then(|path| CatalogCache::load(path).ok())
            .unwrap_or_default();
        Self {
            runner,
            remote: "flathub".to_owned(),
            cache,
            cache_path,
            local_fallback: Vec::new(),
        }
    }

    /// The remote name used by read/install commands.
    pub fn remote(&self) -> &str {
        &self.remote
    }

    /// Change the remote after validating it as a simple Flatpak remote name.
    /// The default is `flathub`; callers normally do not need this method.
    pub fn set_remote(&mut self, remote: &str) -> Result<(), String> {
        validate_remote(remote)?;
        self.remote = remote.to_owned();
        Ok(())
    }

    /// The configured cache path, if disk persistence is available.
    pub fn cache_path(&self) -> Option<&Path> {
        self.cache_path.as_deref()
    }

    /// The last cached entries.
    pub fn cached_entries(&self) -> &[FlatpakApp] {
        self.cache.entries()
    }

    /// Set a trusted local seed for the no-Flatpak fallback. No server data is
    /// synthesized; the integration layer owns the provenance of these rows.
    pub fn set_local_fallback(&mut self, entries: Vec<FlatpakApp>) {
        self.local_fallback = entries;
    }

    /// Builder form of [`Self::set_local_fallback`].
    pub fn with_local_fallback(mut self, entries: Vec<FlatpakApp>) -> Self {
        self.set_local_fallback(entries);
        self
    }

    /// Arguments used for the live remote query. Exposed for audit tests and
    /// integrations that want to display the current query policy.
    pub fn remote_list_args(&self) -> Vec<String> {
        vec![
            "remote-ls".to_owned(),
            "--app".to_owned(),
            format!("--columns={REMOTE_COLUMNS}"),
            self.remote.clone(),
        ]
    }

    /// Refresh the catalogue, falling back to cache/local entries on an
    /// unavailable or unreachable remote.
    pub fn refresh(&mut self) -> CatalogSnapshot {
        let remote_args = self.remote_list_args();
        let remote_result = self.runner.run("flatpak", &remote_args);

        match remote_result {
            Err(error) if error.kind() == io::ErrorKind::NotFound => self.offline_snapshot(
                Vec::new(),
                "Flatpak is not installed; showing only local or cached entries.",
                false,
            ),
            Err(error) => {
                let installed = self.query_installed().unwrap_or_default();
                let message = format!("Flatpak could not query {remote}: {error}", remote = self.remote);
                self.failure_snapshot(installed, message, false)
            }
            Ok(output) if !output.success => {
                let installed = self.query_installed().unwrap_or_default();
                let message = command_message(
                    &output,
                    format!("Flathub remote `{}` did not respond", self.remote),
                );
                self.failure_snapshot(installed, message, looks_offline(&output.stderr))
            }
            Ok(output) => {
                let mut apps = parse_remote_apps(&output.stdout);
                let installed = self.query_installed();
                let mut message = None;
                match installed {
                    Ok(installed) => merge_installed(&mut apps, installed),
                    Err(error) => {
                        message = Some(format!(
                            "Catalogue loaded, but installed Flatpak state was unavailable: {error}"
                        ));
                    }
                }

                self.cache.replace(apps.clone());
                if let Some(path) = self.cache_path.as_deref()
                    && let Err(error) = self.cache.save(path)
                {
                    let cache_message = format!("Catalogue loaded, but cache write failed: {error}");
                    message = Some(match message {
                        Some(existing) => format!("{existing} {cache_message}"),
                        None => cache_message,
                    });
                }

                let state = if apps.is_empty() {
                    CatalogState::Empty
                } else {
                    CatalogState::Ready
                };
                CatalogSnapshot::new(apps, state, CatalogSource::Live, message)
            }
        }
    }

    /// Install one app for the current user.
    pub fn install(&mut self, app_id: &str) -> OperationReport {
        if let Err(message) = validate_app_id(app_id) {
            return rejected_report(OperationKind::Install, Some(app_id), message);
        }
        let args = vec![
            "install".to_owned(),
            "--user".to_owned(),
            "--noninteractive".to_owned(),
            "--assumeyes".to_owned(),
            self.remote.clone(),
            app_id.to_owned(),
        ];
        self.run_operation(OperationKind::Install, Some(app_id), args)
    }

    /// Update one app for the current user.
    pub fn update(&mut self, app_id: &str) -> OperationReport {
        if let Err(message) = validate_app_id(app_id) {
            return rejected_report(OperationKind::Update, Some(app_id), message);
        }
        let args = vec![
            "update".to_owned(),
            "--user".to_owned(),
            "--noninteractive".to_owned(),
            "--assumeyes".to_owned(),
            app_id.to_owned(),
        ];
        self.run_operation(OperationKind::Update, Some(app_id), args)
    }

    /// Update every installed app for the current user.
    pub fn update_all(&mut self) -> OperationReport {
        let args = vec![
            "update".to_owned(),
            "--user".to_owned(),
            "--noninteractive".to_owned(),
            "--assumeyes".to_owned(),
        ];
        self.run_operation(OperationKind::Update, None, args)
    }

    /// Remove one app for the current user.
    pub fn remove(&mut self, app_id: &str) -> OperationReport {
        if let Err(message) = validate_app_id(app_id) {
            return rejected_report(OperationKind::Remove, Some(app_id), message);
        }
        let args = vec![
            "uninstall".to_owned(),
            "--user".to_owned(),
            "--noninteractive".to_owned(),
            "--assumeyes".to_owned(),
            app_id.to_owned(),
        ];
        self.run_operation(OperationKind::Remove, Some(app_id), args)
    }

    fn query_installed(&self) -> Result<Vec<FlatpakApp>, String> {
        let args = vec![
            "list".to_owned(),
            "--app".to_owned(),
            format!("--columns={INSTALLED_COLUMNS}"),
        ];
        match self.runner.run("flatpak", &args) {
            Err(error) => Err(error.to_string()),
            Ok(output) if output.success => Ok(parse_installed_apps(&output.stdout)),
            Ok(output) => Err(command_message(&output, "flatpak list failed".to_owned())),
        }
    }

    fn run_operation(
        &mut self,
        operation: OperationKind,
        app_id: Option<&str>,
        args: Vec<String>,
    ) -> OperationReport {
        let mut command = Vec::with_capacity(args.len() + 1);
        command.push("flatpak".to_owned());
        command.extend(args.iter().cloned());
        match self.runner.run("flatpak", &args) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => OperationReport {
                operation,
                app_id: app_id.map(str::to_owned),
                status: OperationStatus::Unavailable,
                exit_code: None,
                message: "Flatpak is not installed.".to_owned(),
                stdout: String::new(),
                stderr: error.to_string(),
                command,
            },
            Err(error) => OperationReport {
                operation,
                app_id: app_id.map(str::to_owned),
                status: OperationStatus::Failed,
                exit_code: None,
                message: error.to_string(),
                stdout: String::new(),
                stderr: error.to_string(),
                command,
            },
            Ok(output) => {
                let status = if output.success {
                    OperationStatus::Succeeded
                } else {
                    OperationStatus::Failed
                };
                let message = if output.success {
                    format!("{} completed.", operation_label(operation))
                } else {
                    command_message(&output, format!("{} failed", operation_label(operation)))
                };
                if output.success {
                    self.apply_operation_to_cache(operation, app_id);
                }
                OperationReport {
                    operation,
                    app_id: app_id.map(str::to_owned),
                    status,
                    exit_code: output.exit_code,
                    message,
                    stdout: output.stdout,
                    stderr: output.stderr,
                    command,
                }
            }
        }
    }

    fn apply_operation_to_cache(&mut self, operation: OperationKind, app_id: Option<&str>) {
        let Some(app_id) = app_id else {
            if operation == OperationKind::Update {
                for app in &mut self.cache.entries {
                    if app.installed {
                        app.update_available = false;
                    }
                }
                let _ = self.save_cache();
            }
            return;
        };

        if let Some(app) = self.cache.entries.iter_mut().find(|app| app.app_id() == app_id) {
            match operation {
                OperationKind::Install => {
                    app.installed = true;
                    app.update_available = false;
                }
                OperationKind::Update => app.update_available = false,
                OperationKind::Remove => {
                    app.installed = false;
                    app.update_available = false;
                }
            }
        }
        let _ = self.save_cache();
    }

    fn save_cache(&self) -> io::Result<()> {
        match self.cache_path.as_deref() {
            Some(path) => self.cache.save(path),
            None => Ok(()),
        }
    }

    fn offline_snapshot(
        &self,
        installed: Vec<FlatpakApp>,
        message: &str,
        error_state: bool,
    ) -> CatalogSnapshot {
        let mut entries = if self.cache.is_empty() {
            self.local_fallback.clone()
        } else {
            self.cache.entries.clone()
        };
        if !installed.is_empty() {
            merge_installed(&mut entries, installed);
        }
        let source = if !self.cache.is_empty() {
            CatalogSource::Cache
        } else {
            CatalogSource::Local
        };
        let state = if error_state {
            CatalogState::Error(message.to_owned())
        } else {
            CatalogState::Offline
        };
        CatalogSnapshot::new(entries, state, source, Some(message.to_owned()))
    }

    fn failure_snapshot(
        &self,
        installed: Vec<FlatpakApp>,
        message: String,
        offline: bool,
    ) -> CatalogSnapshot {
        self.offline_snapshot(installed, &message, !offline)
    }
}

/// Validate a Flatpak application ID before putting it in an argv vector.
///
/// This is defense in depth: `Command` does not use a shell, but rejecting
/// paths, flags and control characters also prevents accidental operations on
/// an unintended Flatpak ref.
pub fn validate_app_id(app_id: &str) -> Result<(), String> {
    if app_id.is_empty() || app_id.len() > 255 {
        return Err("Flatpak app ID must contain 1..=255 bytes".to_owned());
    }
    if !app_id.contains('.') || app_id.starts_with('-') || app_id.ends_with('.') || app_id.contains("..") {
        return Err("Flatpak app ID must use reverse-DNS components".to_owned());
    }
    for component in app_id.split('.') {
        if component.is_empty() || component.starts_with('-') {
            return Err("Flatpak app ID contains an invalid component".to_owned());
        }
        if !component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err("Flatpak app ID contains unsupported characters".to_owned());
        }
    }
    Ok(())
}

/// Parse `flatpak remote-ls --columns=...` output into app records.
///
/// The first seven tab-separated fields are the stable live-query columns;
/// fields after those are optional and accepted for richer local wrappers.
pub fn parse_remote_apps(output: &str) -> Vec<FlatpakApp> {
    let mut apps = Vec::new();
    let mut seen = BTreeSet::new();
    for line in output.lines() {
        let Some(fields) = split_columns(line) else {
            continue;
        };
        if fields
            .first()
            .is_some_and(|field| field.eq_ignore_ascii_case("application"))
        {
            continue;
        }
        let Some(app) = parse_remote_fields(&fields) else {
            continue;
        };
        if seen.insert(app.app_id().to_owned()) {
            apps.push(app);
        }
    }
    apps
}

/// Parse `flatpak list --app --columns=...` output into installed app records.
pub fn parse_installed_apps(output: &str) -> Vec<FlatpakApp> {
    let mut apps = Vec::new();
    let mut seen = BTreeSet::new();
    for line in output.lines() {
        let Some(fields) = split_columns(line) else {
            continue;
        };
        if fields
            .first()
            .is_some_and(|field| field.eq_ignore_ascii_case("application"))
        {
            continue;
        }
        let Some(app) = parse_installed_fields(&fields) else {
            continue;
        };
        if seen.insert(app.app_id().to_owned()) {
            apps.push(app);
        }
    }
    apps
}

/// Merge installed-state rows into remote rows, preserving remote metadata and
/// retaining installed-only apps as local entries.
pub fn merge_installed(remote: &mut Vec<FlatpakApp>, installed: Vec<FlatpakApp>) {
    for local in installed {
        let Some(existing) = remote.iter_mut().find(|app| app.app_id() == local.app_id()) else {
            remote.push(local);
            continue;
        };

        existing.installed = true;
        existing.update_available = versions_differ(
            existing.metadata.version.as_deref(),
            local.metadata.version.as_deref(),
        );
        fill_missing_metadata(&mut existing.metadata, &local.metadata);
    }
}

fn parse_remote_fields(fields: &[String]) -> Option<FlatpakApp> {
    let app_id = required_field(fields, 0)?;
    if validate_app_id(app_id).is_err() {
        return None;
    }
    // `remote-ls --columns` is tabular. A row that only contains an ID (for
    // example a continuation/metadata line after a wrapped description) is
    // not a usable catalogue card and must not be turned into one by the
    // app-id fallback.
    let name = non_dash(required_field(fields, 1)?)?;
    let summary = non_dash(required_field(fields, 2)?)?;
    let mut metadata = FlatpakMetadata::new(app_id.to_owned(), name, summary);
    metadata.version = optional_field(fields, 3).and_then(non_dash);
    metadata.branch = optional_field(fields, 4).and_then(non_dash);
    metadata.architecture = optional_field(fields, 5).and_then(non_dash);
    metadata.origin = optional_field(fields, 6).and_then(non_dash);
    metadata.runtime = optional_field(fields, 7).and_then(non_dash);
    metadata.license = optional_field(fields, 8).and_then(non_dash);
    metadata.homepage = optional_field(fields, 9).and_then(non_dash);
    metadata.icon = optional_field(fields, 10).and_then(non_dash);
    metadata.categories = optional_field(fields, 11)
        .map(parse_categories)
        .unwrap_or_default();
    metadata.size_bytes = optional_field(fields, 12).and_then(parse_size_bytes);
    Some(FlatpakApp::from_metadata(metadata))
}

fn parse_installed_fields(fields: &[String]) -> Option<FlatpakApp> {
    let app_id = required_field(fields, 0)?;
    if validate_app_id(app_id).is_err() {
        return None;
    }
    let mut metadata = FlatpakMetadata::new(
        app_id.to_owned(),
        optional_field(fields, 1).unwrap_or(app_id).to_owned(),
        String::new(),
    );
    metadata.version = optional_field(fields, 2).and_then(non_dash);
    metadata.branch = optional_field(fields, 3).and_then(non_dash);
    metadata.architecture = optional_field(fields, 4).and_then(non_dash);
    metadata.origin = optional_field(fields, 5).and_then(non_dash);
    metadata.runtime = optional_field(fields, 6).and_then(non_dash);
    metadata.size_bytes = optional_field(fields, 7).and_then(parse_size_bytes);
    let mut app = FlatpakApp::from_metadata(metadata);
    app.installed = true;
    Some(app)
}

fn split_columns(line: &str) -> Option<Vec<String>> {
    let line = line.trim_end_matches('\r');
    if line.trim().is_empty() || !line.contains('\t') {
        return None;
    }
    // Flatpak's --columns output is tab-separated. Ignoring non-tabular lines
    // prevents wrapped descriptions and diagnostics from being misread as
    // additional application rows.
    Some(line.split('\t').map(|field| field.trim().to_owned()).collect())
}

fn required_field(fields: &[String], index: usize) -> Option<&str> {
    fields
        .get(index)
        .map(String::as_str)
        .map(str::trim)
        .filter(|field| !field.is_empty())
}

fn optional_field(fields: &[String], index: usize) -> Option<&str> {
    fields
        .get(index)
        .map(String::as_str)
        .map(str::trim)
        .filter(|field| !field.is_empty())
}

fn non_dash(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value == "-" || value == "?" {
        None
    } else {
        Some(value.to_owned())
    }
}

fn parse_categories(value: &str) -> Vec<String> {
    value.split([';', ',']).filter_map(non_dash).collect()
}

fn parse_size_bytes(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() || value == "-" || value == "?" {
        return None;
    }
    if let Ok(bytes) = value.parse::<u64>() {
        return Some(bytes);
    }

    let mut parts = value.split_whitespace();
    let number = parts.next()?.parse::<f64>().ok()?;
    let unit = parts.next()?.to_ascii_lowercase();
    let multiplier = match unit.as_str() {
        "b" | "byte" | "bytes" => 1.0,
        "kb" | "kib" => 1024.0,
        "mb" | "mib" => 1024.0 * 1024.0,
        "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "tb" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    let bytes = number * multiplier;
    if bytes.is_finite() && bytes >= 0.0 && bytes <= u64::MAX as f64 {
        Some(bytes.round() as u64)
    } else {
        None
    }
}

fn fill_missing_metadata(target: &mut FlatpakMetadata, source: &FlatpakMetadata) {
    if target.name.is_empty() {
        target.name = source.name.clone();
    }
    if target.summary.is_empty() {
        target.summary = source.summary.clone();
    }
    if target.version.is_none() {
        target.version = source.version.clone();
    }
    if target.branch.is_none() {
        target.branch = source.branch.clone();
    }
    if target.architecture.is_none() {
        target.architecture = source.architecture.clone();
    }
    if target.origin.is_none() {
        target.origin = source.origin.clone();
    }
    if target.runtime.is_none() {
        target.runtime = source.runtime.clone();
    }
    if target.size_bytes.is_none() {
        target.size_bytes = source.size_bytes;
    }
}

fn versions_differ(remote: Option<&str>, installed: Option<&str>) -> bool {
    match (remote, installed) {
        (Some(remote), Some(installed)) => remote != installed,
        _ => false,
    }
}

fn validate_remote(remote: &str) -> Result<(), String> {
    if remote.is_empty()
        || remote.len() > 64
        || !remote
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err("Flatpak remote must be a simple name".to_owned());
    }
    Ok(())
}

fn operation_label(operation: OperationKind) -> &'static str {
    match operation {
        OperationKind::Install => "Install",
        OperationKind::Update => "Update",
        OperationKind::Remove => "Remove",
    }
}

fn rejected_report(operation: OperationKind, app_id: Option<&str>, message: String) -> OperationReport {
    OperationReport {
        operation,
        app_id: app_id.map(str::to_owned),
        status: OperationStatus::Rejected,
        exit_code: None,
        message,
        stdout: String::new(),
        stderr: String::new(),
        command: Vec::new(),
    }
}

fn command_message(output: &CommandOutput, fallback: String) -> String {
    let stderr = output.stderr.trim();
    if stderr.is_empty() {
        fallback
    } else {
        format!("{fallback}: {stderr}")
    }
}

fn looks_offline(stderr: &str) -> bool {
    let message = stderr.to_ascii_lowercase();
    [
        "offline",
        "network",
        "connection",
        "resolve",
        "timeout",
        "timed out",
        "dns",
        "download",
        "tls",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.trim().is_empty())
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn encode_cache_app(app: &FlatpakApp) -> String {
    let metadata = &app.metadata;
    let categories = metadata.categories.join("\u{1f}");
    let size = metadata.size_bytes.map(|size| size.to_string());
    let fields = [
        metadata.app_id.as_str(),
        metadata.name.as_str(),
        metadata.summary.as_str(),
        metadata.description.as_str(),
        metadata.version.as_deref().unwrap_or_default(),
        metadata.branch.as_deref().unwrap_or_default(),
        metadata.architecture.as_deref().unwrap_or_default(),
        metadata.origin.as_deref().unwrap_or_default(),
        metadata.runtime.as_deref().unwrap_or_default(),
        metadata.license.as_deref().unwrap_or_default(),
        metadata.homepage.as_deref().unwrap_or_default(),
        metadata.icon.as_deref().unwrap_or_default(),
        categories.as_str(),
        size.as_deref().unwrap_or_default(),
        if app.installed { "1" } else { "0" },
        if app.update_available { "1" } else { "0" },
    ];
    fields
        .iter()
        .map(|field| encode_field(field))
        .collect::<Vec<_>>()
        .join("\t")
}

fn decode_cache_app(line: &str) -> Option<FlatpakApp> {
    let fields: Vec<String> = line.split('\t').map(decode_field).collect::<Option<_>>()?;
    if fields.len() != 16 || validate_app_id(&fields[0]).is_err() {
        return None;
    }
    let mut metadata = FlatpakMetadata::new(fields[0].clone(), fields[1].clone(), fields[2].clone());
    metadata.description = fields[3].clone();
    metadata.version = non_dash(&fields[4]);
    metadata.branch = non_dash(&fields[5]);
    metadata.architecture = non_dash(&fields[6]);
    metadata.origin = non_dash(&fields[7]);
    metadata.runtime = non_dash(&fields[8]);
    metadata.license = non_dash(&fields[9]);
    metadata.homepage = non_dash(&fields[10]);
    metadata.icon = non_dash(&fields[11]);
    metadata.categories = fields[12].split('\u{1f}').filter_map(non_dash).collect();
    metadata.size_bytes = fields[13].parse::<u64>().ok();
    let mut app = FlatpakApp::from_metadata(metadata);
    app.installed = fields[14] == "1";
    app.update_available = fields[15] == "1";
    Some(app)
}

fn encode_field(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0F) as usize] as char);
    }
    encoded
}

fn decode_field(value: &str) -> Option<String> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(value.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        decoded.push((high << 4) | low);
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::VecDeque, fs, path::PathBuf};

    const REMOTE_OUTPUT: &str = "org.example.Editor\tEditor\tA small editor\t1.2\tstable\tx86_64\tflathub\torg.gnome.Platform\tGPL-3.0\thttps://example.invalid/editor\t\tUtility;Development;\t2 MiB\norg.example.Broken\t\t\n";
    const INSTALLED_OUTPUT: &str = "org.example.Editor\tEditor\t1.0\tstable\tx86_64\tflathub\n";

    #[derive(Default)]
    struct FakeRunner {
        results: RefCell<VecDeque<io::Result<CommandOutput>>>,
        calls: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl FakeRunner {
        fn with_results(results: Vec<io::Result<CommandOutput>>) -> Self {
            Self {
                results: RefCell::new(results.into()),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl FlatpakCommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[String]) -> io::Result<CommandOutput> {
            self.calls.borrow_mut().push((program.to_owned(), args.to_vec()));
            self.results
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Ok(CommandOutput::success("")))
        }
    }

    fn temp_cache_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "rouch-app-store-test-{}-{}.tsv",
            std::process::id(),
            unix_seconds()
        ))
    }

    #[test]
    fn remote_and_installed_parsers_keep_optional_metadata() {
        let apps = parse_remote_apps(REMOTE_OUTPUT);
        assert_eq!(apps.len(), 1);
        let app = &apps[0];
        assert_eq!(app.app_id(), "org.example.Editor");
        assert_eq!(app.metadata.version.as_deref(), Some("1.2"));
        assert_eq!(app.metadata.categories, ["Utility", "Development"]);
        assert_eq!(app.metadata.size_bytes, Some(2 * 1024 * 1024));
        assert!(!app.installed);

        let installed = parse_installed_apps(INSTALLED_OUTPUT);
        assert_eq!(installed.len(), 1);
        assert!(installed[0].installed);
        assert_eq!(installed[0].metadata.version.as_deref(), Some("1.0"));
    }

    #[test]
    fn live_refresh_merges_installed_state_and_detects_updates() {
        let runner = FakeRunner::with_results(vec![
            Ok(CommandOutput::success(REMOTE_OUTPUT)),
            Ok(CommandOutput::success(INSTALLED_OUTPUT)),
        ]);
        let mut backend = FlatpakBackend::with_runner_and_cache_path(runner, None);
        let snapshot = backend.refresh();
        assert_eq!(snapshot.source, CatalogSource::Live);
        assert_eq!(snapshot.state, CatalogState::Ready);
        assert_eq!(snapshot.apps.len(), 1);
        assert!(snapshot.apps[0].installed);
        assert!(snapshot.apps[0].update_available);

        let calls = backend.runner.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "flatpak");
        assert_eq!(calls[0].1[0], "remote-ls");
        assert!(
            !calls[0]
                .1
                .iter()
                .any(|arg| arg.contains('|') || arg.contains(';'))
        );
    }

    #[test]
    fn missing_flatpak_uses_trusted_local_fallback_and_reports_offline() {
        let runner = FakeRunner::with_results(vec![Err(io::Error::new(
            io::ErrorKind::NotFound,
            "flatpak not found",
        ))]);
        let fallback = FlatpakApp::new("org.example.Local", "Local app", "Installed metadata");
        let mut backend =
            FlatpakBackend::with_runner_and_cache_path(runner, None).with_local_fallback(vec![fallback]);
        let snapshot = backend.refresh();
        assert_eq!(snapshot.state, CatalogState::Offline);
        assert_eq!(snapshot.source, CatalogSource::Local);
        assert_eq!(snapshot.apps[0].app_id(), "org.example.Local");
        assert!(snapshot.message.as_deref().unwrap().contains("not installed"));
    }

    #[test]
    fn cache_round_trip_preserves_unicode_and_states() {
        let path = temp_cache_path();
        let mut app = FlatpakApp::new("org.example.Editor", "Éditor", "Line\nsummary");
        app.metadata.description = "Description with tabs\tand accents".to_owned();
        app.metadata.categories = vec!["Utility".to_owned(), "Unknown;raw".to_owned()];
        app.metadata.version = Some("1.0".to_owned());
        app.installed = true;
        app.update_available = true;
        let mut cache = CatalogCache::from_entries(vec![app.clone()]);
        cache.replace(vec![app]);
        cache.save(&path).unwrap();

        let loaded = CatalogCache::load(&path).unwrap();
        assert_eq!(loaded.entries(), cache.entries());
        assert!(loaded.saved_at().is_some());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn invalid_ids_are_rejected_before_process_execution() {
        assert!(validate_app_id("org.example.App").is_ok());
        assert!(validate_app_id("org.example.App;rm -rf /").is_err());
        assert!(validate_app_id("--help").is_err());
        assert!(validate_app_id("single-component").is_err());

        let runner = FakeRunner::default();
        let mut backend = FlatpakBackend::with_runner_and_cache_path(runner, None);
        let report = backend.install("org.example.App;bad");
        assert_eq!(report.status, OperationStatus::Rejected);
        assert!(backend.runner.calls.borrow().is_empty());
    }

    #[test]
    fn operations_use_user_scope_and_return_explicit_status() {
        let runner = FakeRunner::with_results(vec![
            Ok(CommandOutput::success("installed")),
            Ok(CommandOutput::failure(Some(1), "permission denied")),
            Err(io::Error::new(io::ErrorKind::NotFound, "missing")),
        ]);
        let mut backend = FlatpakBackend::with_runner_and_cache_path(runner, None);

        let install = backend.install("org.example.App");
        assert_eq!(install.status, OperationStatus::Succeeded);
        assert!(install.command.contains(&"--user".to_owned()));

        let update = backend.update("org.example.App");
        assert_eq!(update.status, OperationStatus::Failed);
        assert!(update.message.contains("permission denied"));

        let remove = backend.remove("org.example.App");
        assert_eq!(remove.status, OperationStatus::Unavailable);
        assert!(remove.is_unavailable());
    }

    #[test]
    fn network_failures_are_offline_but_other_failures_remain_errors() {
        let network_runner = FakeRunner::with_results(vec![Ok(CommandOutput::failure(
            Some(7),
            "Could not resolve host",
        ))]);
        let mut network_backend = FlatpakBackend::with_runner_and_cache_path(network_runner, None);
        let network = network_backend.refresh();
        assert_eq!(network.state, CatalogState::Offline);

        let error_runner =
            FakeRunner::with_results(vec![Ok(CommandOutput::failure(Some(2), "invalid option"))]);
        let mut error_backend = FlatpakBackend::with_runner_and_cache_path(error_runner, None);
        let error = error_backend.refresh();
        assert!(matches!(error.state, CatalogState::Error(_)));
    }
}
