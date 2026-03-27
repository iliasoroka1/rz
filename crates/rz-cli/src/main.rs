//! `rz` — universal inter-agent messaging (cmux + zellij + NATS).

use clap::{Parser, Subcommand};
use eyre::{Result, WrapErr, bail};

use rz_agent_protocol::{Envelope, MessageKind};
use rz_cli::{backend, bootstrap, cmux, log, status};


#[derive(Subcommand)]
enum WorkspaceCmd {
    /// Create a new workspace.
    Create {
        /// Workspace name.
        #[arg(long)]
        name: Option<String>,
        /// Working directory for the new workspace.
        #[arg(long)]
        cwd: Option<String>,
    },
    /// List all workspaces.
    List,
}

/// Universal messaging for AI agents — works in any terminal.
///
/// Quick start:
///   rz run --name worker claude --dangerously-skip-permissions
///   rz send worker "do something"
///   rz list
///
/// Use `rz help <command>` for details. `rz help --all` shows all commands.
#[derive(Parser)]
#[command(name = "rz", version, about, long_about)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Spawn an agent (auto-detects tmux/cmux/zellij, or headless PTY).
    ///
    /// Auto-detects tmux/cmux/zellij. Without a multiplexer, falls back
    /// to a background PTY agent (requires --name and RZ_HUB).
    ///
    /// Examples:
    ///   rz run --name worker claude --dangerously-skip-permissions
    ///   rz run --name coder -p "implement auth" claude --dangerously-skip-permissions
    #[command(alias = "run")]
    Spawn {
        /// Command to run.
        command: String,
        /// Surface name.
        #[arg(short, long)]
        name: Option<String>,
        /// Skip bootstrap instructions.
        #[arg(long)]
        no_bootstrap: bool,
        /// Seconds to wait for process to be ready before bootstrapping.
        #[arg(long, default_value = "45")]
        wait: u64,
        /// Task prompt to send after bootstrap.
        #[arg(short, long)]
        prompt: Option<String>,
        /// Extra arguments passed to the command.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Send a message to an agent.
    ///
    /// Examples:
    ///   rz send worker "do this task"
    ///   rz send lead "DONE: finished the task"
    Send {
        /// Target agent name or ID.
        pane: String,
        /// Message text.
        message: String,
        /// Send plain text instead of @@RZ: envelope.
        #[arg(long)]
        raw: bool,
        /// Override sender identity.
        #[arg(long)]
        from: Option<String>,
        /// Reference a previous message ID (for threading).
        #[arg(long)]
        r#ref: Option<String>,
        /// Block until a reply arrives (timeout in seconds).
        #[arg(long)]
        wait: Option<u64>,
    },

    /// List all agents (local panes + registry + NATS KV).
    #[command(alias = "ps")]
    List,

    /// Run a command as a named rz agent with PTY wrapping (no multiplexer needed).
    ///
    /// Examples:
    ///   rz agent --name worker -- claude --dangerously-skip-permissions
    Agent {
        /// Agent name (used for NATS subject, registry, routing).
        #[arg(long)]
        name: String,
        /// Skip bootstrap instructions.
        #[arg(long)]
        no_bootstrap: bool,
        /// Keep registry entry after exit (for long-running server agents).
        #[arg(long)]
        permanent: bool,
        /// Command and arguments to run (after --).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },

    // ── Less common commands (hidden from default help) ────────

    #[command(hide = true)]
    /// Print this agent's ID.
    Id,

    #[command(hide = true)]
    /// Initialize a shared workspace.
    Init,

    #[command(hide = true)]
    /// Print workspace path.
    Dir,

    #[command(hide = true)]
    /// Send a message and block until the agent replies.
    ///
    /// Shorthand for `rz send --wait 60 <name> "message"`.
    ///
    /// Examples:
    ///   rz ask worker "what is the status?"
    Ask {
        /// Target agent name or ID.
        pane: String,
        /// Message text.
        message: String,
        /// Seconds to wait for a reply (default 60).
        #[arg(long, default_value = "60")]
        timeout: u64,
    },

    #[command(hide = true)]
    /// Collect the last @@RZ: message from each listed agent (MPI-style gather).
    ///
    /// Reads the scrollback of every surface ID given and prints the most
    /// recent protocol message from each — one line per agent. Use this to
    /// fan-in status from a group of parallel workers without dumping each
    /// surface individually.
    ///
    /// Examples:
    ///   rz gather <id1> <id2> <id3>
    ///   rz gather <id1> <id2> --last 3    # last 3 messages from each
    Gather {
        /// Surface IDs to gather from.
        #[arg(required = true)]
        panes: Vec<String>,
        /// Number of recent messages to show per agent (default 1).
        #[arg(long, default_value = "1")]
        last: usize,
    },

    #[command(hide = true)]
    /// Set progress indicator (0.0–1.0). [cmux only]
    ///
    /// Examples:
    ///   rz progress 0.5
    ///   rz progress 0.75 "compiling"
    Progress {
        /// Progress value between 0.0 and 1.0.
        value: f64,
        /// Optional label shown alongside the progress bar.
        label: Option<String>,
    },

    #[command(hide = true)]
    /// Set a status key/value. [cmux only]
    ///
    /// Examples:
    ///   rz status-set build done
    ///   rz status-set phase "running tests" --icon spinner --color "#00ff00"
    #[command(name = "status-set")]
    StatusSet {
        /// Status key.
        key: String,
        /// Status value.
        value: String,
        /// Icon name.
        #[arg(long)]
        icon: Option<String>,
        /// Hex color (e.g. "#ff0000").
        #[arg(long)]
        color: Option<String>,
    },

    #[command(hide = true)]
    /// Clear a status key. [cmux only]
    ///
    /// Examples:
    ///   rz status-clear build
    #[command(name = "status-clear")]
    StatusClear {
        /// Status key to clear.
        key: String,
    },

    #[command(hide = true)]
    /// Fire a named signal.
    ///
    /// Examples:
    ///   rz signal build-done
    Signal {
        /// Signal name to fire.
        name: String,
    },

    #[command(hide = true)]
    /// Block until a named signal fires.
    ///
    /// Examples:
    ///   rz wait-signal build-done
    ///   rz wait-signal build-done --timeout 120
    #[command(name = "wait-signal")]
    WaitSignal {
        /// Signal name to wait for.
        name: String,
        /// Seconds to wait before timing out.
        #[arg(long)]
        timeout: Option<u64>,
    },

    /// Broadcast a message to all other agents.
    Broadcast {
        /// Message text.
        message: String,
        /// Send plain text instead of @@RZ: envelopes.
        #[arg(long)]
        raw: bool,
    },

    /// Show a summary of the session.
    #[command(hide = true)]
    ///
    /// Includes message counts from each surface's scrollback.
    Status,

    /// Dump an agent's scrollback to stdout.
    ///
    /// Alias: `rz logs` (docker-style)
    ///
    /// Examples:
    ///   rz dump worker                    # full scrollback
    ///   rz dump worker --last 50          # last 50 lines only
    #[command(alias = "logs")]
    Dump {
        /// Target agent name or ID.
        pane: String,
        /// Only show the last N lines.
        #[arg(long)]
        last: Option<usize>,
    },

    /// Show @@RZ: protocol messages from an agent's scrollback.
    ///
    /// Extracts and formats all protocol envelopes, filtering out
    /// normal shell output.
    ///
    /// Examples:
    ///   rz log worker
    ///   rz log worker --last 10
    Log {
        /// Target agent name or ID.
        pane: String,
        /// Only show the last N messages.
        #[arg(long)]
        last: Option<usize>,
    },

    /// Close an agent's pane.
    ///
    /// Alias: `rz kill` (docker-style)
    #[command(alias = "kill")]
    Close {
        /// Target agent name or ID.
        pane: String,
    },

    /// Ping an agent and measure round-trip time.
    ///
    /// Sends a Ping envelope and waits for a Pong reply (up to --timeout
    /// seconds). Useful for checking if an agent is alive and responsive.
    ///
    /// Default timeout is 60s — agents may be mid-tool, mid-thought, or
    /// spawning sub-agents and won't respond instantly.
    ///
    /// Examples:
    ///   rz ping worker
    ///   rz ping worker --timeout 120
    Ping {
        /// Target agent name or ID.
        pane: String,
        /// Seconds to wait for a Pong reply.
        #[arg(long, default_value = "60")]
        timeout: u64,
    },

    /// Set a timer — delivers @@RZ: Timer message when it fires.
    ///
    /// Spawns a background thread that sleeps and then sends a Timer
    /// envelope to self when it expires.
    ///
    /// Examples:
    ///   rz timer 30 "check build"     # 30s timer with label
    ///   rz timer 5                     # 5s timer, empty label
    Timer {
        /// Delay in seconds.
        seconds: f64,
        /// Timer label (delivered in the Timer message).
        #[arg(default_value = "")]
        label: String,
    },

    #[command(hide = true)]
    /// Browser automation. [cmux only].
    ///
    /// All arguments are forwarded directly to the cmux browser CLI.
    /// Run `cmux browser help` to see all available subcommands.
    ///
    /// Examples:
    ///   rz browser open-split https://example.com
    ///   rz browser --surface <id> goto https://other.com
    ///   rz browser --surface <id> snap --out /tmp/page.png
    ///   rz browser --surface <id> click "button.submit"
    ///   rz browser --surface <id> type "input#search" "query"
    ///   rz browser --surface <id> wait --load-state complete
    ///   rz browser --surface <id> get text
    ///   rz browser --surface <id> eval "document.title"
    ///   rz browser --surface <id> scroll --dy 500
    ///   rz browser --surface <id> find text "Submit"
    Browser {
        /// Arguments passed directly to `cmux browser`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    #[command(hide = true)]
    /// Send a notification to the user.
    ///
    /// Creates a cmux notification that appears in the sidebar and
    /// as a macOS system notification.
    ///
    /// Examples:
    ///   rz notify "Build complete"
    ///   rz notify "Test failed" --body "3 tests failed in auth module"
    ///   rz notify "Done"
    Notify {
        /// Notification title.
        title: String,
        /// Notification body text.
        #[arg(long)]
        body: Option<String>,
        /// Associate with a specific surface.
        #[arg(long)]
        surface: Option<String>,
    },

    #[command(hide = true)]
    /// Workspace management commands.
    ///
    /// Examples:
    ///   rz workspace create --name "research"
    ///   rz workspace list
    Workspace {
        #[command(subcommand)]
        action: WorkspaceCmd,
    },

    #[command(hide = true)]
    /// Show full system tree. [cmux only].
    ///
    /// Displays the hierarchical structure of the cmux session.
    Tree,

    #[command(hide = true)]
    /// Register this agent in the universal registry.
    ///
    /// Makes this agent discoverable by other agents via `rz ps`.
    /// Transport determines how messages are delivered.
    ///
    /// Examples:
    ///   rz register --name myagent --transport nats
    ///   rz register --name worker --transport file
    ///   rz register --name api --transport http --endpoint http://localhost:7070
    Register {
        /// Agent name.
        #[arg(long)]
        name: String,
        /// Transport type: cmux, file, http.
        #[arg(long, default_value = "file")]
        transport: String,
        /// Transport endpoint (surface ID, URL, etc). Defaults to agent name for file transport.
        #[arg(long)]
        endpoint: Option<String>,
        /// Capabilities (comma-separated).
        #[arg(long)]
        caps: Option<String>,
    },

    #[command(hide = true)]
    /// Remove an agent from the registry.
    Deregister {
        /// Agent name to remove.
        name: String,
    },

    #[command(hide = true)]
    /// Listen for messages on a NATS subject for a named agent.
    ///
    /// Subscribes to the agent's NATS subject and delivers incoming
    /// messages via the specified method. Blocks until interrupted.
    /// Requires the `nats` feature.
    ///
    /// Examples:
    ///   rz listen myagent
    ///   rz listen myagent --deliver file
    ///   rz listen worker --deliver stdout
    
    Listen {
        /// Agent name to listen for.
        name: String,
        /// How to deliver: 'stdout', 'file', 'cmux:<id>', 'zellij:<id>', 'tmux:<id>'.
        #[arg(long, default_value = "stdout")]
        deliver: String,
    },

    #[command(hide = true)]
    /// Receive pending messages from file mailbox.
    ///
    /// Reads and removes messages from ~/.rz/mailboxes/<name>/inbox/.
    /// Prints each as an @@RZ: line.
    ///
    /// Examples:
    ///   rz recv myagent
    ///   rz recv myagent --one     # pop just the oldest message
    ///   rz recv myagent --count   # just show count, don't consume
    Recv {
        /// Agent name (mailbox to read from).
        name: String,
        /// Pop only the oldest message.
        #[arg(long)]
        one: bool,
        /// Just print count without consuming.
        #[arg(long)]
        count: bool,
    },

}

/// Path to the name→UUID registry file.
fn names_path() -> Option<std::path::PathBuf> {
    let ws = workspace_path().ok()?;
    // Ensure the workspace directory exists so names.json can always be written.
    let _ = std::fs::create_dir_all(&ws);
    Some(ws.join("names.json"))
}

/// Load the name→UUID map from disk.
fn load_names() -> std::collections::HashMap<String, String> {
    let Some(path) = names_path() else { return Default::default() };
    let Ok(data) = std::fs::read_to_string(&path) else { return Default::default() };
    serde_json::from_str(&data).unwrap_or_default()
}

/// Save a name→UUID mapping.
fn save_name(name: &str, uuid: &str) {
    let Some(path) = names_path() else { return };
    let mut names = load_names();
    names.insert(name.to_string(), uuid.to_string());
    if let Ok(json) = serde_json::to_string_pretty(&names) {
        let _ = std::fs::write(&path, json);
    }
}

/// Resolve a target: if it looks like a UUID (contains '-'), use as-is.
/// Otherwise look up in the names registry.
/// Resolved target with transport info.
enum Target {
    /// cmux terminal paste (surface_id)
    Cmux(String),
    /// File mailbox (agent name)
    File(String),
    /// HTTP POST (url)
    Http(String),
    /// NATS pub/sub (agent name)
    Nats(String),
}

fn is_uuid(s: &str) -> bool {
    // UUIDs are 36 chars: 8-4-4-4-12 hex digits with dashes
    s.len() == 36
        && s.chars().filter(|c| *c == '-').count() == 4
        && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn resolve_target(target: &str) -> Result<Target> {
    // 1. Universal registry — check and prune dead PTY agents
    if let Ok(Some(entry)) = rz_cli::registry::lookup(target) {
        // Verify PTY agents are still alive (id = "pty-<pid>").
        if let Some(pid_str) = entry.id.strip_prefix("pty-") {
            let alive = std::process::Command::new("kill")
                .args(["-0", pid_str])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !alive {
                let _ = rz_cli::registry::deregister(target);
                // Fall through to other resolution methods.
            } else {
                return registry_target(&entry);
            }
        } else {
            return registry_target(&entry);
        }
    }
    // 1b. NATS KV registry
    if let Ok(Some(entry)) = rz_cli::registry::nats_lookup(target) {
        return registry_target(&entry);
    }

    // 2. cmux/zellij/tmux names
    let names = load_names();
    if let Some(id) = names.get(target) {
        return Ok(Target::Cmux(id.clone()));
    }
    // 3. UUID-like → assume cmux surface ID
    if is_uuid(target) {
        return Ok(Target::Cmux(target.to_string()));
    }
    // 4. Search multiplexer surface titles
    if let Ok(surfaces) = cmux::list_surfaces() {
        for s in &surfaces {
            if s.title.eq_ignore_ascii_case(target) || s.title.contains(target) {
                return Ok(Target::Cmux(s.id.clone()));
            }
        }
    }
    // 5. NATS fallback
    if rz_cli::nats_hub::hub_url().is_some() {
        return Ok(Target::Nats(target.to_string()));
    }
    Err(eyre::eyre!("unknown agent '{}' — use a UUID, a name from `rz run --name`, or `rz register`", target))
}

fn registry_target(entry: &rz_cli::registry::AgentEntry) -> Result<Target> {
    match entry.transport.as_str() {
        "http" => Ok(Target::Http(entry.endpoint.clone())),
        "file" => Ok(Target::File(entry.name.clone())),
        "nats" => Ok(Target::Nats(entry.name.clone())),
        _ => Ok(Target::Cmux(entry.endpoint.clone())),
    }
}

/// Legacy resolver — returns surface ID string for commands that only support cmux.
fn resolve_target_cmux(target: &str) -> Result<String> {
    match resolve_target(target)? {
        Target::Cmux(id) => Ok(id),
        _ => Err(eyre::eyre!("agent '{}' is not a cmux terminal", target)),
    }
}

fn rz_path() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "rz".into())
}

fn dirs_path(subdir: &str) -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home).join(".rz").join(subdir)
}

fn workspace_path() -> Result<std::path::PathBuf> {
    let workspace_id = std::env::var("CMUX_WORKSPACE_ID")
        .map_err(|_| eyre::eyre!("CMUX_WORKSPACE_ID not set — not inside cmux?"))?;
    Ok(std::path::PathBuf::from(format!("/tmp/rz-cmux-{workspace_id}")))
}

/// Spawn a background `rz listen` process for NATS delivery.
/// The child inherits our env so it has multiplexer access.
/// `deliver` should be "cmux:<surface_id>" or "zellij:<pane_id>".
/// Uses a pidfile to avoid duplicate listeners for the same agent name.
fn spawn_nats_listener(rz_path: &str, agent_name: &str, deliver: &str) {
    // Pidfile prevents duplicate listeners per agent name per workspace.
    let session = std::env::var("CMUX_WORKSPACE_ID")
        .or_else(|_| std::env::var("ZELLIJ_SESSION_NAME"))
        .unwrap_or_default();
    let pidfile = std::path::PathBuf::from(format!(
        "/tmp/rz-nats-{}-{}.pid",
        session,
        agent_name,
    ));

    // Check if an existing listener is still alive.
    if let Ok(pid_str) = std::fs::read_to_string(&pidfile) {
        let pid = pid_str.trim();
        if !pid.is_empty() {
            // `kill -0 <pid>` checks if process exists without sending a signal.
            if std::process::Command::new("kill")
                .args(["-0", pid])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
            {
                return; // already running
            }
        }
    }

    match std::process::Command::new(rz_path)
        .args(["listen", agent_name, "--deliver", deliver])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => {
            let _ = std::fs::write(&pidfile, child.id().to_string());
            eprintln!("rz: auto-started NATS listener for '{agent_name}' (pid {})", child.id());
        }
        Err(e) => {
            eprintln!("rz: failed to start NATS listener for '{agent_name}': {e}");
        }
    }
}

fn sender_id(from: Option<&str>) -> String {
    if let Some(f) = from {
        return f.to_string();
    }
    if let Ok(name) = std::env::var("RZ_AGENT_NAME") {
        return name;
    }
    // Look up own ID in names.json to find a human-readable name.
    if let Ok(own_id) = cmux::own_surface_id().or_else(|_| {
        // Try zellij pane ID
        std::env::var("ZELLIJ_PANE_ID").map(|id| format!("terminal_{id}"))
    }) {
        let names = load_names();
        for (name, id) in &names {
            if id == &own_id {
                return name.clone();
            }
        }
        return own_id;
    }
    "unknown".into()
}

/// Poll own scrollback for a reply referencing `msg_id`, with timeout.
fn wait_for_reply(msg_id: &str, timeout_secs: u64) -> Result<()> {
    let own = cmux::own_surface_id()?;
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(timeout_secs);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(250));
        if std::time::Instant::now() >= deadline {
            bail!("timeout ({timeout_secs}s) — no reply to {msg_id}");
        }
        let scrollback = cmux::read_text(&own)?;
        let messages = log::extract_messages(&scrollback);
        if let Some(reply) = messages.iter().rev().find(|m| {
            m.r#ref.as_deref() == Some(msg_id)
        }) {
            println!("{}", log::format_message(reply, None));
            return Ok(());
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Cmd::Id => {
            println!("{}", cmux::own_surface_id()?);
        }

        Cmd::Init => {
            let ws = workspace_path()?;
            std::fs::create_dir_all(ws.join("shared"))?;

            // Create project coordination files (idempotent — don't overwrite).
            let goals = ws.join("goals.md");
            if !goals.exists() {
                std::fs::write(&goals, "\
# Session Goals

> Agents: read this file when you start. Add sub-goals as you discover them.

## Goal
_Fill in the session's primary objective._

## Sub-goals
-

## Completed
-
")?;
            }

            let context = ws.join("context.md");
            if !context.exists() {
                std::fs::write(&context, "\
# Session Context

> Agents: append here, never delete. Prefix entries with the date.

## Decisions

## Discoveries

## Open Questions
-
")?;
            }

            let agents = ws.join("agents.md");
            if !agents.exists() {
                std::fs::write(&agents, "\
# Active Agents

> Agents: update your row when starting or finishing a task.

| Surface | Name | Current Task | Status |
|---------|------|--------------|--------|
")?;
            }

            println!("{}", ws.display());
        }

        Cmd::Dir => {
            let ws = workspace_path()?;
            if !ws.exists() {
                bail!("workspace not initialized — run `rz init` first");
            }
            println!("{}", ws.display());
        }

        Cmd::Spawn {
            command,
            name,
            no_bootstrap,
            wait,
            prompt,
            args,
        } => {
            if let Some(be) = backend::detect() {
                // Inside a multiplexer — spawn a pane.
                let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                let surface_id = be.spawn(&command, &arg_refs, name.as_deref())?;

                if let Some(ref n) = name {
                    save_name(n, &surface_id);
                }

                if let Ok(own) = be.own_id() {
                    save_name("lead", &own);

                    let prefix = be.backend_name();
                    if rz_cli::nats_hub::hub_url().is_some() {
                        let rz = rz_path();
                        if let Some(ref n) = name {
                            spawn_nats_listener(&rz, n, &format!("{prefix}:{surface_id}"));
                        }
                        spawn_nats_listener(&rz, "lead", &format!("{prefix}:{own}"));
                    }
                }

                if !no_bootstrap {
                    be.wait_for_ready(&surface_id, wait, 5);

                    let msg = bootstrap::build(&surface_id, name.as_deref(), be.as_ref())?;
                    be.send(&surface_id, &msg)?;

                    if let Some(task) = prompt {
                        be.wait_for_ready(&surface_id, 30, 3);
                        be.send(&surface_id, &task)?;
                    }
                }

                println!("{surface_id}");
            } else {
                // No multiplexer — fall back to headless PTY agent.
                let agent_name = name.as_deref()
                    .ok_or_else(|| eyre::eyre!("--name is required when spawning without a multiplexer"))?;

                let rz = rz_path();
                let mut cmd_args = vec!["agent".to_string(), "--name".to_string(), agent_name.to_string()];
                if no_bootstrap {
                    cmd_args.push("--no-bootstrap".to_string());
                }
                cmd_args.push("--".to_string());
                cmd_args.push(command.clone());
                for a in &args {
                    cmd_args.push(a.clone());
                }

                // Ensure log directory exists.
                let log_dir = dirs_path("logs");
                let _ = std::fs::create_dir_all(&log_dir);
                let log_file = std::fs::File::create(log_dir.join(format!("{agent_name}.log")))
                    .unwrap_or_else(|_| std::fs::File::create("/dev/null").unwrap());

                let cmd_refs: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();
                match std::process::Command::new(&rz)
                    .args(&cmd_refs)
                    .stdin(std::process::Stdio::null())
                    .stdout(log_file.try_clone().unwrap_or_else(|_| std::fs::File::create("/dev/null").unwrap()))
                    .stderr(log_file)
                    .spawn()
                {
                    Ok(child) => {
                        eprintln!("rz: spawned headless PTY agent '{}' (pid {})", agent_name, child.id());

                        // Send task via NATS after startup delay if prompt given.
                        if let Some(task) = prompt {
                            let target = agent_name.to_string();
                            std::thread::spawn(move || {
                                // Wait for agent to register and start listening.
                                std::thread::sleep(std::time::Duration::from_secs(15));
                                let envelope = Envelope::new(
                                    sender_id(None),
                                    MessageKind::Chat { text: task },
                                ).with_to(&target);
                                if rz_cli::nats_hub::hub_url().is_some() {
                                    let _ = rz_cli::nats_hub::publish(&target, &envelope);
                                }
                            });
                        }

                        println!("pty-{}", child.id());
                    }
                    Err(e) => bail!("failed to spawn headless agent: {e}"),
                }
            }
        }

        Cmd::Send { pane, message, raw, from, r#ref, wait } => {
            let target = resolve_target(&pane)?;

            if raw {
                if wait.is_some() {
                    bail!("--wait requires protocol mode (cannot use with --raw)");
                }
                match &target {
                    Target::Cmux(id) => cmux::send(id, &message)?,
                    _ => bail!("--raw only works with cmux targets"),
                }
            } else {
                let target_id = match &target {
                    Target::Cmux(id) => id.as_str(),
                    Target::Nats(name) | Target::File(name) => name.as_str(),
                    Target::Http(url) => url.as_str(),
                };
                let mut envelope = Envelope::new(
                    sender_id(from.as_deref()),
                    MessageKind::Chat { text: message },
                ).with_to(target_id);
                if let Some(r) = r#ref {
                    envelope = envelope.with_ref(r);
                }
                let msg_id = envelope.id.clone();

                match &target {
                    Target::Cmux(id) => {
                        cmux::send(id, &envelope.encode()?)?;
                    }
                    Target::Nats(name) => {
                        rz_cli::nats_hub::publish(name, &envelope)?;
                    }
                    Target::File(name) => {
                        rz_cli::mailbox::deliver(name, &envelope)?;
                    }
                    Target::Http(url) => {
                        rz_cli::transport::deliver_http(url, &envelope)?;
                    }
                }

                if let Some(timeout_secs) = wait {
                    wait_for_reply(&msg_id, timeout_secs)?;
                }
            }
        }

        Cmd::Ask { pane, message, timeout } => {
            let pane = resolve_target_cmux(&pane)?;
            let from = sender_id(None);
            let envelope = Envelope::new(
                &from,
                MessageKind::Chat { text: message },
            ).with_to(&pane);
            let msg_id = envelope.id.clone();
            cmux::send(&pane, &envelope.encode()?)?;
            wait_for_reply(&msg_id, timeout)?;
        }

        Cmd::Gather { panes, last } => {
            let own = cmux::own_surface_id().ok();
            for pane_ref in &panes {
                let pane = resolve_target_cmux(pane_ref).unwrap_or_else(|_| pane_ref.clone());
                let scrollback = cmux::read_text(&pane).unwrap_or_default();
                let messages = log::extract_messages(&scrollback);
                if messages.is_empty() {
                    println!("{pane_ref}  (no messages)");
                } else {
                    let start = messages.len().saturating_sub(last);
                    for msg in &messages[start..] {
                        println!("{pane_ref}  {}", log::format_message(msg, own.as_deref()));
                    }
                }
            }
        }

        Cmd::Progress { value, label } => {
            let mut cmd = std::process::Command::new("cmux");
            cmd.arg("set-progress").arg(value.to_string());
            if let Some(l) = &label { cmd.arg("--label").arg(l); }
            let status = cmd
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .status()
                .wrap_err("cmux set-progress failed")?;
            if !status.success() { std::process::exit(status.code().unwrap_or(1)); }
        }

        Cmd::StatusSet { key, value, icon, color } => {
            let mut cmd = std::process::Command::new("cmux");
            cmd.arg("set-status").arg(&key).arg(&value);
            if let Some(i) = &icon { cmd.arg("--icon").arg(i); }
            if let Some(c) = &color { cmd.arg("--color").arg(c); }
            let status = cmd
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .status()
                .wrap_err("cmux set-status failed")?;
            if !status.success() { std::process::exit(status.code().unwrap_or(1)); }
        }

        Cmd::StatusClear { key } => {
            let mut cmd = std::process::Command::new("cmux");
            cmd.arg("clear-status").arg(&key);
            let status = cmd
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .status()
                .wrap_err("cmux clear-status failed")?;
            if !status.success() { std::process::exit(status.code().unwrap_or(1)); }
        }

        Cmd::Signal { name } => {
            let status = std::process::Command::new("cmux")
                .arg("wait-for")
                .arg("-S")
                .arg(&name)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .status()
                .wrap_err("cmux wait-for -S failed")?;
            if !status.success() { std::process::exit(status.code().unwrap_or(1)); }
        }

        Cmd::WaitSignal { name, timeout } => {
            let mut cmd = std::process::Command::new("cmux");
            cmd.arg("wait-for").arg(&name);
            if let Some(t) = timeout { cmd.arg("--timeout").arg(t.to_string()); }
            let status = cmd
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .status()
                .wrap_err("cmux wait-for failed")?;
            if !status.success() { std::process::exit(status.code().unwrap_or(1)); }
        }

        Cmd::Broadcast { message, raw } => {
            let from = sender_id(None);
            let peers = cmux::list_surface_ids()?;
            let own = cmux::own_surface_id().ok();
            let mut sent = 0;

            for peer in &peers {
                if own.as_deref() == Some(peer.as_str()) {
                    continue;
                }
                if raw {
                    cmux::send(peer, &message)?;
                } else {
                    let envelope = Envelope::new(
                        &from,
                        MessageKind::Chat { text: message.clone() },
                    );
                    cmux::send(peer, &envelope.encode()?)?;
                }
                sent += 1;
            }
            eprintln!("broadcast to {sent} surfaces");
        }

        Cmd::List => {
            println!("{:<18} {:<38} {:<20} {:<8}",
                "NAME", "ID", "TITLE", "TYPE");

            // Show multiplexer panes if inside one.
            let own = cmux::own_surface_id().ok();
            if let Ok(surfaces) = cmux::list_surfaces() {
                let names = load_names();
                let uuid_to_name: std::collections::HashMap<&str, &str> = names
                    .iter()
                    .map(|(n, u)| (u.as_str(), n.as_str()))
                    .collect();
                for s in &surfaces {
                    let marker = if own.as_deref() == Some(s.id.as_str()) { " (me)" } else { "" };
                    let title = if s.title.is_empty() { "-" } else { &s.title };
                    let name = uuid_to_name.get(s.id.as_str()).unwrap_or(&"-");
                    println!("{:<18} {:<38} {:<20} {:<8}{}",
                        name, s.id, title, s.surface_type, marker);
                }
            }

            // Also show agents from the universal registry (PTY agents, etc.)
            let mut shown_names = std::collections::HashSet::new();
            if let Ok(agents) = rz_cli::registry::list_all() {
                let own_name = std::env::var("RZ_AGENT_NAME").ok();
                for a in &agents {
                    let marker = if own_name.as_deref() == Some(a.name.as_str()) { " (me)" } else { "" };
                    println!("{:<18} {:<38} {:<20} {:<8}{}",
                        a.name, a.id, a.transport, "agent", marker);
                    shown_names.insert(a.name.clone());
                }
            }

            // Also show agents from NATS KV (global view)
            if let Ok(nats_agents) = rz_cli::registry::nats_list() {
                for a in &nats_agents {
                    if !shown_names.contains(&a.name) {
                        println!("{:<18} {:<38} {:<20} {:<8} (nats)",
                            a.name, a.id, a.transport, "agent");
                    }
                }
            }
        }

        Cmd::Status => {
            let surfaces = cmux::list_surfaces()?;
            let summary = status::summarize(&surfaces, |id| cmux::read_text(id).ok());
            print!("{}", status::format_summary(&summary));
        }

        Cmd::Dump { pane, last } => {
            let pane = resolve_target_cmux(&pane)?;
            let text = cmux::read_text(&pane)?;
            if let Some(n) = last {
                let lines: Vec<&str> = text.lines().collect();
                let skip = lines.len().saturating_sub(n);
                for line in lines.into_iter().skip(skip) {
                    println!("{}", line);
                }
            } else {
                print!("{}", text);
            }
        }

        Cmd::Log { pane, last } => {
            let pane = resolve_target_cmux(&pane)?;
            let own = cmux::own_surface_id().ok();
            let scrollback = cmux::read_text(&pane)?;
            let mut messages = log::extract_messages(&scrollback);
            if let Some(n) = last {
                let skip = messages.len().saturating_sub(n);
                messages = messages.into_iter().skip(skip).collect();
            }
            for msg in &messages {
                println!("{}", log::format_message(msg, own.as_deref()));
            }
        }

        Cmd::Close { pane } => {
            let pane = resolve_target_cmux(&pane)?;
            cmux::close(&pane)?;
        }

        Cmd::Ping { pane, timeout } => {
            let pane = resolve_target_cmux(&pane)?;
            let own = cmux::own_surface_id()?;
            let from = sender_id(None);
            let envelope = Envelope::new(&from, MessageKind::Ping);
            let ping_id = envelope.id.clone();
            let sent = std::time::Instant::now();

            cmux::send(&pane, &envelope.encode()?)?;

            let deadline = sent + std::time::Duration::from_secs(timeout);
            loop {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if std::time::Instant::now() >= deadline {
                    println!("timeout ({timeout}s) — no pong from {pane}");
                    std::process::exit(1);
                }
                let scrollback = cmux::read_text(&own)?;
                let messages = log::extract_messages(&scrollback);
                let got_pong = messages.iter().any(|m| {
                    matches!(m.kind, MessageKind::Pong)
                        && m.r#ref.as_deref() == Some(&ping_id)
                });
                if got_pong {
                    let rtt = sent.elapsed();
                    println!("pong from {pane} in {:.1}ms", rtt.as_secs_f64() * 1000.0);
                    break;
                }
            }
        }

        Cmd::Timer { seconds, label } => {
            let own = cmux::own_surface_id()?;
            let encoded = Envelope::new("timer", MessageKind::Timer { label }).encode()?;

            // Spawn a detached child process: sleep then send the timer envelope to self.
            let script = format!(
                "sleep {} && {} send --raw {} {}",
                seconds,
                shell_escape(&rz_path()),
                shell_escape(&own),
                shell_escape(&encoded),
            );

            std::process::Command::new("sh")
                .args(["-c", &script])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?;

            eprintln!("timer set for {seconds}s");
        }

        Cmd::Browser { args } => {
            let status = std::process::Command::new("cmux")
                .arg("browser")
                .args(&args)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .status()
                .wrap_err("failed to run `cmux browser` — is cmux in PATH?")?;
            if !status.success() {
                std::process::exit(status.code().unwrap_or(1));
            }
        }

        Cmd::Notify { title, body, surface } => {
            cmux::notify(&title, body.as_deref(), surface.as_deref())?;
        }

        Cmd::Workspace { action } => {
            match action {
                WorkspaceCmd::Create { name, cwd } => {
                    let ws_id = cmux::workspace_create(name.as_deref(), cwd.as_deref())?;
                    println!("{ws_id}");
                }
                WorkspaceCmd::List => {
                    let result = cmux::workspace_list()?;
                    println!("{}", serde_json::to_string_pretty(&result)?);
                }
            }
        }

        Cmd::Tree => {
            let result = cmux::system_tree()?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }

        Cmd::Register { name, transport, endpoint, caps } => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let ep = endpoint.unwrap_or_else(|| name.clone());
            let capabilities = caps
                .map(|c| c.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();
            let id = cmux::own_surface_id().unwrap_or_else(|_| name.clone());
            let entry = rz_cli::registry::AgentEntry {
                name: name.clone(),
                id,
                transport,
                endpoint: ep,
                capabilities,
                permanent: false,
                registered_at: now,
                last_seen: now,
            };
            rz_cli::registry::register(entry.clone())?;
            let _ = rz_cli::registry::nats_register(&entry);
            println!("registered: {}", name);
        }

        Cmd::Deregister { name } => {
            rz_cli::registry::deregister(&name)?;
            let _ = rz_cli::registry::nats_deregister(&name);
            println!("deregistered: {}", name);
        }

        
        Cmd::Listen { name, deliver } => {
            rz_cli::nats_hub::subscribe_and_deliver(&name, &deliver)?;
        }

        Cmd::Recv { name, one, count } => {
            if count {
                let n = rz_cli::mailbox::count(&name)?;
                println!("{}", n);
            } else if one {
                match rz_cli::mailbox::receive_one(&name)? {
                    Some(env) => println!("{}", env.encode()?),
                    None => std::process::exit(1), // no messages
                }
            } else {
                let messages = rz_cli::mailbox::receive(&name)?;
                if messages.is_empty() {
                    std::process::exit(1);
                }
                for env in &messages {
                    println!("{}", env.encode()?);
                }
            }
        }

        Cmd::Agent { name, no_bootstrap, permanent, command } => {
            rz_cli::pty::run_agent(&name, &command, no_bootstrap, permanent)?;
        }
    }

    Ok(())
}

/// Simple shell escaping for single-quoted strings.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

