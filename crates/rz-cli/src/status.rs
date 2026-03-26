//! Surface status summary for the `status` subcommand.

use crate::cmux::SurfaceInfo;
use rz_agent_protocol::SENTINEL;

/// Per-surface status line.
pub struct SurfaceStatus {
    pub surface_id: String,
    pub title: String,
    pub command: String,
    pub running: bool,
    pub message_count: usize,
}

/// Summary of all surfaces.
pub struct StatusSummary {
    pub total: usize,
    pub running: usize,
    pub exited: usize,
    pub surfaces: Vec<SurfaceStatus>,
}

/// Count `@@RZ:` lines in a scrollback string.
fn count_messages(scrollback: &str) -> usize {
    scrollback.lines().filter(|l| l.contains(SENTINEL)).count()
}

/// Build a [`StatusSummary`] from a list of surfaces and a function that provides
/// each surface's scrollback.
///
/// The caller supplies `get_scrollback` so the function stays testable without
/// hitting real cmux — in production, pass `|id| rz::cmux::dump(id)`.
pub fn summarize(
    surfaces: &[SurfaceInfo],
    get_scrollback: impl Fn(&str) -> Option<String>,
) -> StatusSummary {
    let mut running = 0usize;
    let exited = 0usize;
    let mut statuses = Vec::with_capacity(surfaces.len());

    for surface in surfaces {
        // cmux surfaces are always running (no exit tracking)
        running += 1;

        let msg_count = get_scrollback(&surface.id)
            .map(|s| count_messages(&s))
            .unwrap_or(0);

        statuses.push(SurfaceStatus {
            surface_id: surface.id.clone(),
            title: surface.title.clone(),
            command: "-".to_string(),
            running: true,
            message_count: msg_count,
        });
    }

    StatusSummary {
        total: surfaces.len(),
        running,
        exited,
        surfaces: statuses,
    }
}

/// Format the summary as a human-readable string.
pub fn format_summary(summary: &StatusSummary) -> String {
    let mut out = format!(
        "{} surfaces ({} running, {} exited)\n",
        summary.total, summary.running, summary.exited,
    );

    for s in &summary.surfaces {
        let state = if s.running {
            "running".to_string()
        } else {
            "exited".to_string()
        };
        out.push_str(&format!(
            "  {} | {} | {} | {} | {} msgs\n",
            s.surface_id, s.title, s.command, state, s.message_count,
        ));
    }

    out
}
