//! Agent registry — tracks all terminal panes in the session.
//!
//! Merges two sources: explicit registration via pipe messages and
//! automatic discovery via PaneUpdate events from Zellij.

use std::collections::{BTreeMap, BTreeSet};
use serde::Serialize;
use zellij_tile::prelude::*;

/// Ticks of no message activity before Active → Idle.
const IDLE_THRESHOLD: u64 = 60;

/// Ticks after death before an entry is pruned.
const PRUNE_AFTER_TICKS: u64 = 120;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Lifecycle state of an agent pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AgentStatus {
    Active,
    Idle,
    Dead {
        exit_status: Option<i32>,
        died_at: u64,
    },
    Pending,
}

/// A single agent tracked by the registry.
#[derive(Debug, Clone, Serialize)]
pub struct AgentEntry {
    pub pane_id: u32,
    pub name: Option<String>,
    pub capabilities: Vec<String>,
    pub status: AgentStatus,
    pub registered: bool,
    pub created_at: u64,
    pub last_seen: u64,
    pub last_message: u64,
    pub command: Option<String>,
    pub title: String,
    pub tab: usize,
    pub is_floating: bool,
}

#[derive(Debug)]
pub enum RegistryError {
    NameConflict { name: String, existing_pane: u32 },
    NotFound { pane_id: u32 },
}

impl core::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NameConflict { name, existing_pane } => {
                write!(f, "name '{name}' already taken by terminal_{existing_pane}")
            }
            Self::NotFound { pane_id } => {
                write!(f, "no agent with pane_id terminal_{pane_id}")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Central registry of all agent panes in the session.
pub struct AgentRegistry {
    agents: BTreeMap<u32, AgentEntry>,
    tick: u64,
    name_index: BTreeMap<String, u32>,
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self {
            agents: BTreeMap::new(),
            tick: 0,
            name_index: BTreeMap::new(),
        }
    }
}

impl AgentRegistry {
    // -- Registration (from pipe messages) ------------------------------------

    /// Register an agent explicitly. Upgrades an existing entry or creates
    /// a Pending one if the pane hasn't appeared in PaneManifest yet.
    pub fn register(
        &mut self,
        pane_id: u32,
        name: String,
        capabilities: Vec<String>,
    ) -> Result<&AgentEntry, RegistryError> {
        // Check name uniqueness.
        if let Some(&existing) = self.name_index.get(&name) {
            if existing != pane_id {
                return Err(RegistryError::NameConflict {
                    name,
                    existing_pane: existing,
                });
            }
        }

        let tick = self.tick;
        let entry = self.agents.entry(pane_id).or_insert_with(|| AgentEntry {
            pane_id,
            name: None,
            capabilities: Vec::new(),
            status: AgentStatus::Pending,
            registered: false,
            created_at: tick,
            last_seen: tick,
            last_message: 0,
            command: None,
            title: String::new(),
            tab: 0,
            is_floating: false,
        });

        // Remove old name mapping if re-registering with a different name.
        if let Some(ref old_name) = entry.name {
            if *old_name != name {
                self.name_index.remove(old_name);
            }
        }

        entry.name = Some(name.clone());
        entry.capabilities = capabilities;
        entry.registered = true;

        self.name_index.insert(name, pane_id);

        Ok(&self.agents[&pane_id])
    }

    /// Remove an agent's explicit registration. Retains the entry if the
    /// pane is still alive (reverts to unregistered), removes it if Dead.
    pub fn unregister(&mut self, pane_id: u32) -> Result<(), RegistryError> {
        let entry = self
            .agents
            .get_mut(&pane_id)
            .ok_or(RegistryError::NotFound { pane_id })?;

        if let Some(ref name) = entry.name {
            self.name_index.remove(name);
        }

        if matches!(entry.status, AgentStatus::Dead { .. }) {
            self.agents.remove(&pane_id);
        } else {
            entry.name = None;
            entry.capabilities.clear();
            entry.registered = false;
        }

        Ok(())
    }

    // -- PaneUpdate reconciliation --------------------------------------------

    /// Reconcile the registry with a fresh PaneManifest from Zellij.
    pub fn update_from_pane_manifest(&mut self, manifest: &PaneManifest) {
        self.tick += 1;
        let current_tick = self.tick;

        let mut seen: BTreeSet<u32> = BTreeSet::new();

        for (tab_index, panes) in &manifest.panes {
            for pane in panes {
                if pane.is_plugin {
                    continue;
                }
                if !pane.is_selectable {
                    continue;
                }

                seen.insert(pane.id);

                if let Some(entry) = self.agents.get_mut(&pane.id) {
                    // Known agent — update from manifest.
                    entry.title = pane.title.clone();
                    entry.command = pane.terminal_command.clone();
                    entry.tab = *tab_index;
                    entry.is_floating = pane.is_floating;
                    entry.last_seen = current_tick;

                    if pane.exited {
                        entry.status = AgentStatus::Dead {
                            exit_status: pane.exit_status,
                            died_at: current_tick,
                        };
                    } else if matches!(
                        entry.status,
                        AgentStatus::Pending | AgentStatus::Dead { .. }
                    ) {
                        entry.status = AgentStatus::Active;
                    }
                } else if !pane.exited {
                    // Unknown, alive pane — auto-discover.
                    self.agents.insert(
                        pane.id,
                        AgentEntry {
                            pane_id: pane.id,
                            name: None,
                            capabilities: Vec::new(),
                            status: AgentStatus::Active,
                            registered: false,
                            created_at: current_tick,
                            last_seen: current_tick,
                            last_message: 0,
                            command: pane.terminal_command.clone(),
                            title: pane.title.clone(),
                            tab: *tab_index,
                            is_floating: pane.is_floating,
                        },
                    );
                }
            }
        }

        // Known agents not in manifest → mark Dead.
        for (_pane_id, entry) in &mut self.agents {
            if !seen.contains(&entry.pane_id)
                && !matches!(entry.status, AgentStatus::Dead { .. })
            {
                entry.status = AgentStatus::Dead {
                    exit_status: None,
                    died_at: current_tick,
                };
            }
        }

        self.check_idle();
        self.prune_dead();
        self.rebuild_name_index();
    }

    /// Record that a message was routed to/from this agent.
    pub fn touch_message(&mut self, pane_id: u32) {
        if let Some(entry) = self.agents.get_mut(&pane_id) {
            entry.last_message = self.tick;
            if entry.status == AgentStatus::Idle {
                entry.status = AgentStatus::Active;
            }
        }
    }

    /// Transition agents with no recent message activity to Idle.
    fn check_idle(&mut self) {
        let tick = self.tick;
        for entry in self.agents.values_mut() {
            if entry.status == AgentStatus::Active
                && entry.last_message > 0
                && tick.saturating_sub(entry.last_message) > IDLE_THRESHOLD
            {
                entry.status = AgentStatus::Idle;
            }
        }
    }

    /// Remove Dead agents that have been dead long enough.
    fn prune_dead(&mut self) {
        let tick = self.tick;
        self.agents.retain(|_id, entry| {
            if let AgentStatus::Dead { died_at, .. } = &entry.status {
                tick.saturating_sub(*died_at) <= PRUNE_AFTER_TICKS
            } else {
                true
            }
        });
    }

    // -- Queries --------------------------------------------------------------

    pub fn lookup_by_name(&self, name: &str) -> Option<&AgentEntry> {
        self.name_index
            .get(name)
            .and_then(|id| self.agents.get(id))
    }

    pub fn get_active(&self) -> Vec<&AgentEntry> {
        self.agents
            .values()
            .filter(|e| matches!(e.status, AgentStatus::Active | AgentStatus::Idle))
            .collect()
    }

    pub fn get_all(&self) -> Vec<&AgentEntry> {
        self.agents.values().collect()
    }

    pub fn tick(&self) -> u64 {
        self.tick
    }

    // -- Internal -------------------------------------------------------------

    fn rebuild_name_index(&mut self) {
        self.name_index.clear();
        for (id, entry) in &self.agents {
            if let Some(ref name) = entry.name {
                self.name_index.insert(name.clone(), *id);
            }
        }
    }
}
