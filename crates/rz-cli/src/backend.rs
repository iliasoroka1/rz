//! Unified Backend trait abstracting over cmux and zellij transports.

use eyre::Result;

// ---------------------------------------------------------------------------
// PaneInfo — normalised view of a surface/pane across backends
// ---------------------------------------------------------------------------

/// Backend-agnostic pane/surface descriptor.
#[derive(Debug, Clone)]
pub struct PaneInfo {
    pub id: String,
    pub title: String,
    pub is_plugin: bool,
    pub is_focused: bool,
    pub running: bool,
}

// ---------------------------------------------------------------------------
// Backend trait
// ---------------------------------------------------------------------------

pub trait Backend {
    /// This agent's own pane/surface ID.
    fn own_id(&self) -> Result<String>;

    /// Send text to `target` pane (paste + Enter).
    fn send(&self, target: &str, text: &str) -> Result<()>;

    /// Spawn a command in a new pane. Returns the new pane ID.
    fn spawn(&self, cmd: &str, args: &[&str], name: Option<&str>) -> Result<String>;

    /// Close a pane by ID.
    fn close(&self, target: &str) -> Result<()>;

    /// List all panes with metadata.
    fn list_panes(&self) -> Result<Vec<PaneInfo>>;

    /// List IDs of terminal (non-plugin) panes only.
    fn list_pane_ids(&self) -> Result<Vec<String>>;

    /// Read full scrollback from a pane.
    fn read_scrollback(&self, target: &str) -> Result<String>;

    /// Normalise a user-supplied pane identifier into canonical form.
    fn normalize_id(&self, input: &str) -> String;

    /// Block until `target` has output, then settle.
    fn wait_for_ready(&self, target: &str, max_secs: u64, settle_secs: u64);

    /// Name of the session / workspace.
    fn session_name(&self) -> Result<String>;

    /// Backend identifier: `"cmux"` or `"zellij"`.
    fn backend_name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// CmuxBackend
// ---------------------------------------------------------------------------

pub struct CmuxBackend;

impl Backend for CmuxBackend {
    fn own_id(&self) -> Result<String> {
        crate::cmux::own_surface_id()
    }

    fn send(&self, target: &str, text: &str) -> Result<()> {
        crate::cmux::send(target, text)
    }

    fn spawn(&self, cmd: &str, args: &[&str], name: Option<&str>) -> Result<String> {
        crate::cmux::spawn(cmd, args, name)
    }

    fn close(&self, target: &str) -> Result<()> {
        crate::cmux::close(target)
    }

    fn list_panes(&self) -> Result<Vec<PaneInfo>> {
        let surfaces = crate::cmux::list_surfaces()?;
        Ok(surfaces
            .into_iter()
            .map(|s| PaneInfo {
                id: s.id,
                title: s.title,
                is_plugin: s.surface_type != "terminal",
                is_focused: s.is_focused,
                running: true, // cmux surfaces are always running while listed
            })
            .collect())
    }

    fn list_pane_ids(&self) -> Result<Vec<String>> {
        crate::cmux::list_surface_ids()
    }

    fn read_scrollback(&self, target: &str) -> Result<String> {
        crate::cmux::read_text(target)
    }

    fn normalize_id(&self, input: &str) -> String {
        // cmux IDs are UUIDs — pass through as-is
        input.to_string()
    }

    fn wait_for_ready(&self, target: &str, max_secs: u64, settle_secs: u64) {
        crate::cmux::wait_for_stable_output(target, max_secs, settle_secs);
    }

    fn session_name(&self) -> Result<String> {
        std::env::var("CMUX_WORKSPACE_ID")
            .map_err(|_| eyre::eyre!("CMUX_WORKSPACE_ID not set"))
    }

    fn backend_name(&self) -> &str {
        "cmux"
    }
}

// ---------------------------------------------------------------------------
// ZellijBackend
// ---------------------------------------------------------------------------

pub struct ZellijBackend;

impl Backend for ZellijBackend {
    fn own_id(&self) -> Result<String> {
        crate::zellij::own_pane_id()
    }

    fn send(&self, target: &str, text: &str) -> Result<()> {
        crate::zellij::send(target, text)
    }

    fn spawn(&self, cmd: &str, args: &[&str], name: Option<&str>) -> Result<String> {
        crate::zellij::spawn(cmd, args, name)
    }

    fn close(&self, target: &str) -> Result<()> {
        crate::zellij::close(target)
    }

    fn list_panes(&self) -> Result<Vec<PaneInfo>> {
        let panes = crate::zellij::list_panes()?;
        Ok(panes
            .into_iter()
            .map(|p| PaneInfo {
                id: p.pane_id(),
                title: p.title,
                is_plugin: p.is_plugin,
                is_focused: p.is_focused,
                running: !p.exited,
            })
            .collect())
    }

    fn list_pane_ids(&self) -> Result<Vec<String>> {
        crate::zellij::list_pane_ids()
    }

    fn read_scrollback(&self, target: &str) -> Result<String> {
        crate::zellij::dump(target)
    }

    fn normalize_id(&self, input: &str) -> String {
        crate::zellij::normalize_pane_id(input)
    }

    fn wait_for_ready(&self, _target: &str, _max_secs: u64, settle_secs: u64) {
        // Zellij `run` blocks until the pane is ready; just settle.
        std::thread::sleep(std::time::Duration::from_secs(settle_secs));
    }

    fn session_name(&self) -> Result<String> {
        std::env::var("ZELLIJ_SESSION_NAME")
            .map_err(|_| eyre::eyre!("ZELLIJ_SESSION_NAME not set"))
    }

    fn backend_name(&self) -> &str {
        "zellij"
    }
}

// ---------------------------------------------------------------------------
// Auto-detect
// ---------------------------------------------------------------------------

/// Detect the active backend from environment variables.
///
/// Returns `Some(CmuxBackend)` if `CMUX_SURFACE_ID` is set,
/// `Some(ZellijBackend)` if `ZELLIJ` is set, or `None` otherwise.
pub fn detect() -> Option<Box<dyn Backend>> {
    if std::env::var("CMUX_SURFACE_ID").is_ok() {
        return Some(Box::new(CmuxBackend));
    }
    if std::env::var("ZELLIJ").is_ok() {
        return Some(Box::new(ZellijBackend));
    }
    None
}
