use std::collections::BTreeMap;
use zellij_tile::prelude::*;

mod registry;
mod router;

pub struct PendingTimer {
    pub id: u64,
    pub pane_id: u32,
    pub label: String,
    pub seconds: f64,
}

struct RzHub {
    registry: registry::AgentRegistry,
    dirty: bool,
    timers: Vec<PendingTimer>,
    next_timer_id: u64,
}

impl Default for RzHub {
    fn default() -> Self {
        Self {
            registry: registry::AgentRegistry::default(),
            dirty: false,
            timers: Vec::new(),
            next_timer_id: 1,
        }
    }
}

register_plugin!(RzHub);

impl ZellijPlugin for RzHub {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::WriteToStdin,
            PermissionType::ReadCliPipes,
        ]);

        subscribe(&[
            EventType::PaneUpdate,
            EventType::PermissionRequestResult,
            EventType::Timer,
        ]);

        if configuration.get("visible").map(|v| v.as_str()) != Some("true") {
            hide_self();
        }
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PaneUpdate(manifest) => {
                self.registry.update_from_pane_manifest(&manifest);
                self.dirty = true;
            }
            Event::Timer(elapsed) => {
                // Match and remove only the FIRST timer with matching duration.
                // Each set_timeout() produces its own Event::Timer, so we must
                // consume exactly one PendingTimer per event.
                if let Some(idx) = self.timers.iter().position(|t| (t.seconds - elapsed).abs() < 0.01) {
                    let timer = self.timers.swap_remove(idx);
                    router::deliver_timer(timer.pane_id, &timer.label);
                }
            }
            Event::PermissionRequestResult(_) => {}
            _ => {}
        }
        self.dirty
    }

    fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
        if pipe_message.name != "rz" {
            return false;
        }
        router::handle_pipe(
            &mut self.registry,
            &pipe_message,
            &mut self.timers,
            &mut self.next_timer_id,
        );
        self.dirty = matches!(
            pipe_message.args.get("action").map(|s| s.as_str()),
            Some("register" | "unregister")
        );
        self.dirty
    }

    fn render(&mut self, _rows: usize, _cols: usize) {
        self.dirty = false;
        // Dashboard rendering skipped in v1.
    }
}
