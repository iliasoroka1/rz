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

#[derive(Subcommand)]
enum KvCmd {
    /// Get a value from a KV bucket.
    Get {
        /// Bucket name (e.g. rz-issues, rz-agents).
        bucket: String,
        /// Key to look up.
        key: String,
    },
    /// List all keys in a KV bucket.
    List {
        /// Bucket name.
        bucket: String,
    },
    /// Watch a KV bucket for changes (live stream).
    Watch {
        /// Bucket name.
        bucket: String,
        /// Optional key to watch (watches all keys if omitted).
        key: Option<String>,
    },
}

/// Universal messaging for AI agents — works in any terminal.
#[derive(Parser)]
#[command(
    name = "rz",
    version,
    about,
    after_help = "Quick start:\n  rz run --name worker claude --dangerously-skip-permissions\n  rz send worker \"do something\"\n  rz ps\n\nUse `rz help <command>` for details. `rz help --all` shows all commands."
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start an agent (auto-detects tmux/cmux/zellij, or headless PTY).
    ///
    /// Auto-detects tmux/cmux/zellij. Without a multiplexer, falls back
    /// to a background PTY agent (requires --name and RZ_HUB).
    ///
    /// Examples:
    ///   rz run --name worker claude --dangerously-skip-permissions
    ///   rz run --name coder -p "implement auth" claude --dangerously-skip-permissions
    #[command(alias = "spawn")]
    Run {
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

    #[command(verbatim_doc_comment)]
    /// Send a message to an agent.
    ///
    /// Examples:
    ///   rz send worker "do this task"
    ///   rz send lead "DONE: finished the task"
    ///   rz send lead - <<'EOF'
    ///   DONE: markdown with `backticks` and $vars, sent verbatim
    ///   EOF
    Send {
        /// Target agent name or ID.
        pane: String,
        /// Message text. Omit it or pass "-" to read it from stdin: the safe way to
        /// send markdown, since the shell won't touch backticks or $ in a quoted heredoc.
        message: Option<String>,
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
        /// Publish to NATS even if no live listener is found.
        /// The message is stored durably and delivered when the agent reconnects.
        #[arg(long)]
        force: bool,
    },

    /// Show all agents (local panes + registry + NATS KV).
    #[command(alias = "list")]
    Ps,

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

    /// Bridge a Telegram chat to NATS — makes a Telegram chat appear as an rz agent.
    Telegram {
        /// Telegram Bot API token (or set RZ_TELEGRAM_TOKEN env var).
        #[arg(long, env = "RZ_TELEGRAM_TOKEN")]
        token: String,
        /// Telegram chat ID to bridge.
        #[arg(long, env = "RZ_TELEGRAM_CHAT_ID")]
        chat_id: i64,
        /// Agent name for this bridge.
        #[arg(long, default_value = "telegram")]
        name: String,
        /// Default agent to route messages without @prefix.
        #[arg(long, default_value = "lead")]
        default_agent: String,
    },

    /// Bridge an HTTP agent to NATS — lets HTTP services send/receive rz messages.
    ///
    /// Subscribes to NATS for inbound messages and POSTs them to the webhook URL.
    /// Exposes a local HTTP endpoint for the agent to send messages back.
    ///
    /// Examples:
    ///   rz bridge --name api-bot --webhook http://localhost:7070/inbox
    ///   rz bridge --name api-bot --webhook http://localhost:7070/inbox --port 7071
    Bridge {
        /// Agent name (used for NATS subject and registry).
        #[arg(long)]
        name: String,
        /// URL to POST incoming messages to (the HTTP agent's inbox).
        #[arg(long)]
        webhook: String,
        /// Port for the bridge's outbound HTTP server (default 7071).
        #[arg(long, default_value = "7071")]
        port: u16,
        /// Keep registry entry after exit.
        #[arg(long)]
        permanent: bool,
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
    /// Note: in a multiplexer the reply also arrives in your pane as an
    /// @@RZ: line (that's the transport) — this command additionally
    /// prints it so scripts can capture the answer synchronously.
    ///
    /// Examples:
    ///   rz ask worker "what is the status?"
    Ask {
        /// Target agent name or ID.
        pane: String,
        /// Message text, or "-"/omitted to read it from stdin.
        message: Option<String>,
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
        /// Message text, or "-"/omitted to read it from stdin.
        message: Option<String>,
        /// Send plain text instead of @@RZ: envelopes.
        #[arg(long)]
        raw: bool,
    },

    /// Show a summary of the session.
    #[command(hide = true)]
    ///
    /// Includes message counts from each surface's scrollback.
    Status,

    /// Show an agent's scrollback.
    ///
    /// Examples:
    ///   rz logs worker
    ///   rz logs worker --last 50
    #[command(alias = "dump")]
    Logs {
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

    /// Stop an agent.
    #[command(alias = "close")]
    Kill {
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

    /// Delegate a task to another agent (creates an issue via Interstellar bridge).
    ///
    /// Examples:
    ///   rz delegate worker "implement auth module"
    ///   rz delegate worker "fix login bug" --description "Users report 500 on /login"
    Delegate {
        /// Target agent name.
        target: String,
        /// Task title/description.
        task: String,
        /// Optional detailed description.
        #[arg(long)]
        description: Option<String>,
    },

    /// Claim an issue for yourself.
    ///
    /// Examples:
    ///   rz claim ISSUE-123
    Claim {
        /// Issue ID to claim.
        issue_id: String,
    },

    /// Release a claimed issue back to the backlog.
    ///
    /// Examples:
    ///   rz release ISSUE-123
    ///   rz release ISSUE-123 --reason "need more context"
    Release {
        /// Issue ID to release.
        issue_id: String,
        /// Reason for releasing.
        #[arg(long)]
        reason: Option<String>,
    },

    /// Mark an issue as complete.
    ///
    /// Examples:
    ///   rz complete ISSUE-123
    ///   rz complete ISSUE-123 --summary "Added auth middleware and tests"
    Complete {
        /// Issue ID completed.
        issue_id: String,
        /// Summary of what was done.
        #[arg(long)]
        summary: Option<String>,
    },

    /// Report an issue as blocked.
    ///
    /// Examples:
    ///   rz block ISSUE-123 --reason "waiting on API keys"
    Block {
        /// Issue ID that is blocked.
        issue_id: String,
        /// What is blocking progress.
        #[arg(long)]
        reason: Option<String>,
    },

    /// Hand off an issue to another agent with context.
    ///
    /// Examples:
    ///   rz handoff worker ISSUE-123 --summary "Auth logic done, needs frontend"
    Handoff {
        /// Target agent to hand off to.
        target: String,
        /// Issue ID to hand off.
        issue_id: String,
        /// Context/summary for the receiving agent.
        #[arg(long)]
        summary: Option<String>,
    },

    /// Emit a structured envelope with any custom kind. The universal primitive.
    ///
    /// The receiver decides what to do with it. Interstellar's bridge reacts
    /// to known kinds (delegate, claim, etc). Any system can define its own.
    ///
    /// Body can be JSON or key=value pairs (auto-converted to JSON object).
    ///
    /// Examples:
    ///   rz emit worker delegate '{"title":"Fix the bug"}'
    ///   rz emit self claim issueId=IST-42
    ///   rz emit ark knowledge.search query="auth tokens" limit=5
    ///   rz emit monitor alert level=critical message="disk full"
    Emit {
        /// Target agent name (use "self" to address yourself).
        target: String,
        /// Message kind (e.g. delegate, claim, alert, knowledge.search).
        kind: String,
        /// Body as JSON string or key=value pairs.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        body: Vec<String>,
    },

    /// Read from NATS KV buckets (requires RZ_HUB).
    ///
    /// Examples:
    ///   rz kv get rz-issues IST-42
    ///   rz kv list rz-issues
    ///   rz kv watch rz-issues
    #[command(subcommand)]
    Kv(KvCmd),

    /// Sweep orphaned NATS state (requires RZ_HUB).
    ///
    /// Removes: legacy unscoped streams (pre-session format), current-session
    /// streams whose agent is no longer registered, and stale KV entries.
    /// Other sessions' streams are never touched. Dry-run by default.
    ///
    /// Examples:
    ///   rz gc          # list what would be removed
    ///   rz gc --yes    # actually remove it
    Gc {
        /// Actually delete (default is a dry-run listing).
        #[arg(long)]
        yes: bool,
    },
}

/// Path to the name→UUID registry file, scoped by session so a process
/// with an `RZ_SESSION` override can't resolve another session's names.
fn names_path() -> Option<std::path::PathBuf> {
    let sid = rz_cli::session::session_id();
    let sanitized: String = sid
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let dir = std::path::PathBuf::from(format!("/tmp/rz-session-{sanitized}"));
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("names.json"))
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

/// Check whether a local process is alive via `kill -0`.
fn process_alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Decide whether a registry entry points at a dead agent.
///
/// PTY agents and listeners carry their pid ("pty-<pid>" / "listen-<pid>"),
/// so local ones can be liveness-checked directly — this catches `kill -9`'d
/// listeners immediately instead of waiting out the staleness window.
/// Everything else falls back to the last_seen threshold.
fn entry_is_dead(entry: &rz_cli::registry::AgentEntry) -> bool {
    let pid = entry
        .id
        .strip_prefix("pty-")
        .or_else(|| entry.id.strip_prefix("listen-"));
    match pid {
        Some(pid_str) if rz_cli::registry::is_local(entry) => !process_alive(pid_str),
        _ => {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            rz_cli::registry::is_stale(entry, now_ms)
        }
    }
}

fn resolve_target(target: &str) -> Result<Target> {
    // 1. Universal registry — prune dead or stale entries before trusting them.
    if let Ok(Some(entry)) = rz_cli::registry::lookup(target) {
        if entry_is_dead(&entry) {
            let _ = rz_cli::registry::deregister(target);
            // Fall through to other resolution methods.
        } else {
            return registry_target(&entry);
        }
    }
    // 1b. NATS KV registry — same treatment; a SIGKILL'd listener never
    // deregisters itself, so a blind hit here would black-hole messages.
    if let Ok(Some(entry)) = rz_cli::registry::nats_lookup(target) {
        if entry_is_dead(&entry) {
            let _ = rz_cli::registry::nats_deregister(target);
        } else {
            return registry_target(&entry);
        }
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
    // 5. Nothing matched. Publishing blindly to NATS here would durably store
    //    a message no consumer will ever read — fail loudly instead.
    if rz_cli::nats_hub::hub_url().is_some() {
        return Err(eyre::eyre!(
            "unknown agent '{target}' — no live listener found (checked registry, NATS KV, names, surfaces).\n\
             Live agents: {}.\n\
             If the agent is temporarily offline, use `rz send {target} <msg> --force` to store the message on NATS until it reconnects.",
            live_agent_names().join(", ")
        ));
    }
    Err(eyre::eyre!("unknown agent '{}' — use a UUID, a name from `rz run --name`, or `rz register`", target))
}

/// Names of agents that look alive, for "who can I send to" hints in errors.
fn live_agent_names() -> Vec<String> {
    let mut names: Vec<String> = rz_cli::registry::list_all()
        .unwrap_or_default()
        .into_iter()
        .filter(|a| !entry_is_dead(a))
        .map(|a| a.name)
        .collect();
    names.extend(load_names().into_keys());
    names.sort();
    names.dedup();
    names
}

/// Deliver `envelope` to `target`. Returns a short receipt ("via cmux:<id>").
/// A transport failure names the target and lists live agents, so a stale
/// registry entry ("Workspace not found") is not a dead end.
fn deliver(name: &str, target: &Target, envelope: &Envelope) -> Result<String> {
    let (via, result) = match target {
        Target::Cmux(id) => (format!("cmux:{id}"), envelope.encode().and_then(|e| cmux::send(id, &e))),
        Target::Nats(n) => (format!("nats:{n}"), rz_cli::nats_hub::publish(n, envelope)),
        Target::File(n) => (format!("file:{n}"), rz_cli::mailbox::deliver(n, envelope)),
        Target::Http(url) => (format!("http:{url}"), rz_cli::transport::deliver_http(url, envelope)),
    };
    match result {
        Ok(()) => Ok(via),
        Err(e) => {
            let live = live_agent_names();
            let live = if live.is_empty() { "none".to_string() } else { live.join(", ") };
            Err(eyre::eyre!(
                "could not deliver to '{name}' via {via}: {e}\n\
                 The registry entry may be stale (its terminal or workspace is gone). Live agents: {live}"
            ))
        }
    }
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
    if let Ok(Target::Cmux(id)) = resolve_target(target) {
        return Ok(id);
    }
    // An agent spawned by `rz run` is both a NATS listener (registry) and a
    // local pane (names.json). The registry entry wins in resolve_target, so
    // pane-only commands (logs/kill) must fall back to the pane mapping.
    let names = load_names();
    if let Some(id) = names.get(target) {
        return Ok(id.clone());
    }
    if let Ok(surfaces) = cmux::list_surfaces() {
        for s in &surfaces {
            if s.title.eq_ignore_ascii_case(target) || s.title.contains(target) {
                return Ok(s.id.clone());
            }
        }
    }
    Err(eyre::eyre!("agent '{}' is not a cmux terminal", target))
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
    // If a live listener for this name is already registered (e.g. one the
    // user started themselves), don't spawn a competing one — two consumers
    // on the same durable split the message stream.
    if let Ok(Some(e)) = rz_cli::registry::lookup(agent_name) {
        if e.transport == "nats" {
            let alive = e
                .id
                .strip_prefix("listen-")
                .map(process_alive)
                .unwrap_or(false);
            if alive {
                return;
            }
        }
    }

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

    // Log to a file instead of /dev/null — cmux delivery failures (e.g.
    // socket access denied for detached processes) are invisible otherwise.
    let log_dir = dirs_path("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log = std::fs::File::create(log_dir.join(format!("listen-{agent_name}.log"))).ok();
    let (out, err) = match log {
        Some(f) => (
            f.try_clone().map(std::process::Stdio::from).unwrap_or_else(|_| std::process::Stdio::null()),
            std::process::Stdio::from(f),
        ),
        None => (std::process::Stdio::null(), std::process::Stdio::null()),
    };

    match std::process::Command::new(rz_path)
        .args(["listen", agent_name, "--deliver", deliver])
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err)
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

/// Get the sender name from RZ_AGENT_NAME or fall back to sender_id().
fn sender_name() -> String {
    std::env::var("RZ_AGENT_NAME").unwrap_or_else(|_| sender_id(None))
}

/// Wait for a reply referencing `msg_id`, wherever it can arrive.
/// In cmux, poll our own scrollback (catches both cmux-pasted and
/// listener-delivered replies). Headless, watch our own NATS subject.
fn wait_for_reply_any(msg_id: &str, timeout_secs: u64) -> Result<()> {
    if cmux::own_surface_id().is_ok() {
        return wait_for_reply(msg_id, timeout_secs);
    }
    let me = sender_name();
    let reply = rz_cli::nats_hub::wait_for_ref(
        &me,
        msg_id,
        std::time::Duration::from_secs(timeout_secs),
    )?;
    println!("{}", log::format_message(&reply, None));
    Ok(())
}

/// Parse key=value pairs into a JSON object.
/// Values that look like numbers become numbers, "true"/"false" become bools.
fn parse_kv_body(args: &[String]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for arg in args {
        if let Some((key, val)) = arg.split_once('=') {
            let json_val = if val == "true" {
                serde_json::Value::Bool(true)
            } else if val == "false" {
                serde_json::Value::Bool(false)
            } else if let Ok(n) = val.parse::<i64>() {
                serde_json::Value::Number(n.into())
            } else if let Ok(f) = val.parse::<f64>() {
                serde_json::json!(f)
            } else {
                serde_json::Value::String(val.to_string())
            };
            map.insert(key.to_string(), json_val);
        }
    }
    serde_json::Value::Object(map)
}

/// Publish a structured envelope with a custom kind/body as raw JSON.
///
/// Builds the full `@@RZ:{...}` wire format manually so we can use
/// arbitrary kind names that may not exist in the MessageKind enum.
fn publish_structured(target_name: &str, from: &str, kind_name: &str, body: serde_json::Value) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    let id = rz_agent_protocol::message_id(ts, seq as u32);

    let envelope = serde_json::json!({
        "id": id,
        "from": from,
        "to": target_name,
        "kind": { "kind": kind_name, "body": body },
        "ts": ts,
    });
    let wire = format!("@@RZ:{}", envelope.to_string());
    rz_cli::nats_hub::publish_raw(target_name, wire.as_bytes())?;
    Ok(())
}

/// The message from the argument, or from stdin when it is omitted or "-".
///
/// Agents write markdown, and inside a double-quoted shell argument bash runs
/// `backticks` and expands $vars, silently dropping parts of the message
/// before rz ever sees it. A quoted heredoc (`rz send lead - <<'EOF'`) passes
/// the text through untouched.
fn message_text(arg: Option<String>) -> Result<String> {
    use std::io::{IsTerminal, Read};

    if let Some(m) = arg.filter(|m| m != "-") {
        return Ok(m);
    }
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        bail!("no message given: pass it as an argument, or on stdin: rz send <name> - <<'EOF' ... EOF");
    }
    let mut buf = String::new();
    stdin.read_to_string(&mut buf).wrap_err("reading message from stdin")?;
    stdin_message(&buf)
}

/// Heredocs and `echo` add a trailing newline; keep everything else verbatim.
fn stdin_message(buf: &str) -> Result<String> {
    let text = buf.trim_end_matches(['\n', '\r']);
    if text.trim().is_empty() {
        bail!("empty message on stdin");
    }
    Ok(text.to_string())
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

        Cmd::Run {
            command,
            name,
            no_bootstrap,
            wait,
            prompt,
            args,
        } => {
            if let Some(be) = backend::detect() {
                // Inside a multiplexer — spawn a pane.
                // Split pane WITHOUT command first, so we can type the listener
                // before the main command starts.
                let surface_id = be.spawn("", &[], name.as_deref())?;

                if let Some(ref n) = name {
                    save_name(n, &surface_id);
                }

                if let Ok(own) = be.own_id() {
                    save_name("lead", &own);

                    let prefix = be.backend_name();
                    if rz_cli::nats_hub::hub_url().is_some() {
                        // Type the listener command INTO the pane's shell BEFORE
                        // the main command. It runs as a background job with
                        // full cmux access since it's a child of the pane's shell.
                        if let Some(ref n) = name {
                            let rz = rz_path();
                            let listen_cmd = format!(
                                "{rz} listen {n} --deliver {prefix}:{surface_id} &>/dev/null &",
                            );
                            // The pane's shell may still be initializing and
                            // swallow the typed command. Listeners register
                            // themselves, so verify registration and retry
                            // once instead of trusting a fixed sleep.
                            let mut listener_up = false;
                            'attempts: for attempt in 0..2 {
                                if attempt > 0 {
                                    eprintln!("rz: listener for '{n}' not up yet, retyping...");
                                }
                                cmux::send(&surface_id, &listen_cmd)?;
                                for _ in 0..10 {
                                    std::thread::sleep(std::time::Duration::from_millis(500));
                                    if let Ok(Some(e)) = rz_cli::registry::lookup(n) {
                                        let alive = e
                                            .id
                                            .strip_prefix("listen-")
                                            .map(process_alive)
                                            .unwrap_or(false);
                                        if alive {
                                            listener_up = true;
                                            break 'attempts;
                                        }
                                    }
                                }
                            }
                            if !listener_up {
                                eprintln!(
                                    "rz: warning: could not verify listener for '{n}' — \
                                     inbound NATS messages may not reach it"
                                );
                            }
                        }
                        // Also start listener for lead (own pane)
                        let rz = rz_path();
                        spawn_nats_listener(&rz, "lead", &format!("{prefix}:{own}"));
                    }
                }

                // NOW type the main command into the pane
                {
                    let mut full_cmd = cmux::shell_escape_arg(&command);
                    for arg in &args {
                        full_cmd.push(' ');
                        full_cmd.push_str(&cmux::shell_escape_arg(arg));
                    }
                    cmux::send(&surface_id, &full_cmd)?;
                }

                if !no_bootstrap {
                    be.wait_for_ready(&surface_id, wait, 5);

                    // A listener was typed into the pane's shell above whenever
                    // NATS is configured and the agent has a name.
                    let listener_running =
                        rz_cli::nats_hub::hub_url().is_some() && name.is_some();
                    let msg = bootstrap::build(
                        &surface_id,
                        name.as_deref(),
                        be.as_ref(),
                        listener_running,
                    )?;
                    be.send(&surface_id, &msg)?;

                    if let Some(task) = prompt {
                        be.wait_for_ready(&surface_id, 30, 3);
                        // Send the task as an @@RZ envelope, not plain text —
                        // the agent needs sender attribution to distinguish
                        // agent traffic from a human typing in its terminal.
                        let envelope = Envelope::new(
                            sender_id(None),
                            MessageKind::Chat { text: task },
                        )
                        .with_to(name.as_deref().unwrap_or(&surface_id));
                        be.send(&surface_id, &envelope.encode()?)?;
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

        Cmd::Send { pane, message, raw, from, r#ref, wait, force } => {
            let message = message_text(message)?;
            let target = if force {
                if rz_cli::nats_hub::hub_url().is_none() {
                    bail!("--force requires RZ_HUB to be set (it publishes to NATS)");
                }
                Target::Nats(pane.clone())
            } else {
                resolve_target(&pane)?
            };

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

                let via = deliver(&pane, &target, &envelope)?;
                // Receipt on stderr so scripts reading stdout are unaffected.
                eprintln!("sent {msg_id} to {pane} via {via}");

                if let Some(timeout_secs) = wait {
                    wait_for_reply_any(&msg_id, timeout_secs)?;
                }
            }
        }

        Cmd::Ask { pane, message, timeout } => {
            let message = message_text(message)?;
            let target = resolve_target(&pane)?;
            let from = sender_id(None);
            let envelope = Envelope::new(
                &from,
                MessageKind::Chat { text: message },
            ).with_to(&pane);
            let msg_id = envelope.id.clone();
            deliver(&pane, &target, &envelope)?;
            wait_for_reply_any(&msg_id, timeout)?;
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
            let message = message_text(message)?;
            let from = sender_id(None);
            let own_surface = cmux::own_surface_id().ok();
            let own_name = std::env::var("RZ_AGENT_NAME").ok();
            let names = load_names();
            let mut sent = 0;

            // Registered session agents receive via NATS routing — this
            // covers headless agents and works outside a multiplexer too.
            let mut nats_targets = std::collections::BTreeSet::new();
            if !raw {
                if let Ok(agents) = rz_cli::registry::list_all() {
                    nats_targets.extend(agents.into_iter().map(|a| a.name));
                }
                if let Ok(agents) = rz_cli::registry::nats_list() {
                    nats_targets.extend(agents.into_iter().map(|a| a.name));
                }
                if let Some(n) = &own_name {
                    nats_targets.remove(n);
                }
                nats_targets.remove(&from);
            }

            // Surfaces owned by a NATS target get the message through their
            // listener — don't paste a duplicate into their terminal.
            let covered: std::collections::HashSet<&String> =
                nats_targets.iter().filter_map(|n| names.get(n)).collect();

            if let Ok(peers) = cmux::list_surface_ids() {
                for peer in &peers {
                    if own_surface.as_deref() == Some(peer.as_str()) {
                        continue;
                    }
                    if covered.contains(peer) {
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
            }

            for name in &nats_targets {
                let envelope = Envelope::new(
                    &from,
                    MessageKind::Chat { text: message.clone() },
                )
                .with_to(name);
                match rz_cli::nats_hub::publish(name, &envelope) {
                    Ok(_) => sent += 1,
                    Err(e) => eprintln!("rz: broadcast to '{name}' failed: {e}"),
                }
            }
            eprintln!("broadcast to {sent} agents");
        }

        Cmd::Ps => {
            // Drop entries that stopped heartbeating so dead agents don't linger.
            let _ = rz_cli::registry::cleanup_stale(600);

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

        Cmd::Logs { pane, last } => {
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

        Cmd::Kill { pane } => {
            let pane = resolve_target_cmux(&pane)?;
            cmux::close(&pane)?;
        }

        Cmd::Ping { pane, timeout } => {
            // NATS agents get an infrastructure-level ping: their listener
            // answers over request/reply without involving the agent at all.
            if let Ok(Target::Nats(name)) = resolve_target(&pane) {
                let rtt = rz_cli::nats_hub::ping(
                    &name,
                    std::time::Duration::from_secs(timeout),
                )?;
                println!("pong from {name} in {:.1}ms", rtt.as_secs_f64() * 1000.0);
                return Ok(());
            }

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
                host: rz_cli::registry::local_host(),
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
            // Register the agent so `rz ps` and `rz send` can discover it.
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let entry = rz_cli::registry::AgentEntry {
                name: name.clone(),
                id: format!("listen-{}", std::process::id()),
                transport: "nats".to_string(),
                endpoint: name.clone(),
                capabilities: vec![],
                permanent: false,
                registered_at: now_ms,
                last_seen: now_ms,
                host: rz_cli::registry::local_host(),
            };
            rz_cli::registry::register(entry.clone())?;
            let _ = rz_cli::registry::nats_register(&entry);

            // Heartbeat so registries don't consider a long-lived listener stale.
            // Re-register the full entry (not just last_seen): a competing
            // listener may have overwritten it with its own pid, and after it
            // dies the entry must heal back to point at us.
            // A listener whose pane was closed would otherwise look alive
            // forever (and keep the name routed to a dead pane), so it exits
            // once cmux says the surface is gone.
            let hb_name = name.clone();
            let hb_entry = entry.clone();
            let hb_surface = deliver.strip_prefix("cmux:").map(str::to_owned);
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(120));
                if let Some(surface) = &hb_surface {
                    if cmux::surface_exists(surface) == Some(false) {
                        eprintln!("rz: pane {surface} for '{hb_name}' is gone, stopping listener");
                        let _ = rz_cli::registry::deregister(&hb_name);
                        let _ = rz_cli::registry::nats_deregister(&hb_name);
                        std::process::exit(0);
                    }
                }
                let mut e = hb_entry.clone();
                e.last_seen = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let _ = rz_cli::registry::register(e);
                let _ = rz_cli::registry::nats_heartbeat(&hb_name, &hb_entry);
            });

            // When delivering to cmux, update names.json so `rz ps` shows
            // the NATS name instead of whatever the surface was previously called.
            if let Some(surface_id) = deliver.strip_prefix("cmux:") {
                save_name(&name, surface_id);
            }

            // Block on NATS subscription; deregister on exit.
            let result = rz_cli::nats_hub::subscribe_and_deliver(&name, &deliver);

            let _ = rz_cli::registry::deregister(&name);
            let _ = rz_cli::registry::nats_deregister(&name);

            result?;
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

        Cmd::Delegate { target, task, description } => {
            let from = sender_name();
            let mut body = serde_json::json!({ "title": task });
            if let Some(desc) = description {
                body["description"] = serde_json::Value::String(desc);
            }
            publish_structured(&target, &from, "delegate", body)?;
            eprintln!("delegated to {target}: {task}");
        }

        Cmd::Claim { issue_id } => {
            let from = sender_name();
            let body = serde_json::json!({ "issueId": issue_id });
            publish_structured(&from, &from, "claim", body)?;
            eprintln!("claimed issue {issue_id}");
        }

        Cmd::Release { issue_id, reason } => {
            let from = sender_name();
            let mut body = serde_json::json!({ "issueId": issue_id });
            if let Some(r) = reason {
                body["reason"] = serde_json::Value::String(r);
            }
            publish_structured(&from, &from, "release", body)?;
            eprintln!("released issue {issue_id}");
        }

        Cmd::Complete { issue_id, summary } => {
            let from = sender_name();
            let mut body = serde_json::json!({ "issueId": issue_id });
            if let Some(s) = summary {
                body["summary"] = serde_json::Value::String(s);
            }
            publish_structured(&from, &from, "task_complete", body)?;
            eprintln!("completed issue {issue_id}");
        }

        Cmd::Block { issue_id, reason } => {
            let from = sender_name();
            let mut body = serde_json::json!({ "issueId": issue_id });
            if let Some(r) = reason {
                body["reason"] = serde_json::Value::String(r);
            }
            publish_structured(&from, &from, "blocked", body)?;
            eprintln!("blocked issue {issue_id}");
        }

        Cmd::Handoff { target, issue_id, summary } => {
            let from = sender_name();
            let mut body = serde_json::json!({ "issueId": issue_id });
            if let Some(s) = summary {
                body["summary"] = serde_json::Value::String(s);
            }
            publish_structured(&target, &from, "handoff", body)?;
            eprintln!("handed off issue {issue_id} to {target}");
        }

        Cmd::Emit { target, kind, body } => {
            let from = sender_name();
            let actual_target = if target == "self" { from.clone() } else { target };

            // Parse body: try as single JSON string first, then as key=value pairs
            let body_json = if body.len() == 1 {
                // Try parsing as JSON
                match serde_json::from_str::<serde_json::Value>(&body[0]) {
                    Ok(v) => v,
                    Err(_) => {
                        // Not JSON — try as key=value
                        parse_kv_body(&body)
                    }
                }
            } else if body.is_empty() {
                serde_json::json!({})
            } else {
                parse_kv_body(&body)
            };

            publish_structured(&actual_target, &from, &kind, body_json)?;
            eprintln!("emitted {kind} to {actual_target}");
        }

        Cmd::Agent { name, no_bootstrap, permanent, command } => {
            rz_cli::pty::run_agent(&name, &command, no_bootstrap, permanent)?;
        }

        Cmd::Telegram { token, chat_id, name, default_agent } => {
            let rt = tokio::runtime::Runtime::new()
                .wrap_err("tokio runtime")?;
            rt.block_on(async {
                let hub_url = rz_cli::nats_hub::hub_url()
                    .ok_or_else(|| eyre::eyre!("RZ_HUB must be set for Telegram bridge"))?;
                let client = async_nats::connect(&hub_url).await
                    .wrap_err("NATS connect")?;
                let js = async_nats::jetstream::new(client.clone());
                let kv = js.create_key_value(async_nats::jetstream::kv::Config {
                    bucket: "rz-agents".into(),
                    ..Default::default()
                }).await.ok();
                let bridge = rz_cli::telegram::TelegramBridge::new(
                    token, chat_id, name, default_agent,
                );
                bridge.run(&client, kv.as_ref()).await
            })?;
        }

        Cmd::Bridge { name, webhook, port, permanent } => {
            rz_cli::bridge::run_bridge(&name, &webhook, port, permanent)?;
        }

        Cmd::Gc { yes } => {
            if rz_cli::nats_hub::hub_url().is_none() {
                bail!("RZ_HUB must be set for gc");
            }
            let sid = rz_cli::session::session_id();
            let registered: std::collections::HashSet<String> = {
                let mut names = std::collections::HashSet::new();
                if let Ok(agents) = rz_cli::registry::list_all() {
                    names.extend(agents.into_iter().map(|a| a.name));
                }
                if let Ok(agents) = rz_cli::registry::nats_list() {
                    names.extend(agents.into_iter().map(|a| a.name));
                }
                names
            };

            let client = rz_cli::nats_hub::client()?;
            let mut removed = 0usize;
            rz_cli::nats_hub::runtime().block_on(async {
                use futures::StreamExt;
                let js = async_nats::jetstream::new(client);

                // Streams: legacy unscoped (agent.<name>) or current-session
                // orphans (agent.<sid>.<name> with no registered agent).
                let mut streams = js.streams();
                let mut candidates: Vec<(String, String, u64)> = Vec::new();
                while let Some(info) = streams.next().await {
                    let Ok(info) = info else { continue };
                    let name = info.config.name.clone();
                    if !name.starts_with("RZ_") {
                        continue;
                    }
                    let Some(subj) = info.config.subjects.first() else { continue };
                    let tokens: Vec<&str> = subj.split('.').collect();
                    let reason = match tokens.as_slice() {
                        ["agent", _n] => Some("legacy unscoped stream"),
                        ["agent", s, n] if *s == sid && !registered.contains(*n) => {
                            Some("no registered agent in this session")
                        }
                        _ => None,
                    };
                    if let Some(reason) = reason {
                        candidates.push((name, reason.to_string(), info.state.messages));
                    }
                }
                for (name, reason, msgs) in &candidates {
                    if yes {
                        match js.delete_stream(name).await {
                            Ok(_) => {
                                println!("removed stream {name} ({reason}, {msgs} msgs dropped)");
                                removed += 1;
                            }
                            Err(e) => eprintln!("rz: gc: delete {name} failed: {e}"),
                        }
                    } else {
                        println!("would remove stream {name} ({reason}, {msgs} msgs)");
                    }
                }

                // KV: legacy unscoped keys (no '.') and stale entries.
                if let Ok(store) = js.get_key_value("rz-agents").await {
                    if let Ok(mut keys) = store.keys().await {
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        while let Some(Ok(key)) = keys.next().await {
                            let legacy = !key.contains('.');
                            let stale = match store.get(&key).await {
                                Ok(Some(v)) => serde_json::from_slice::<rz_cli::registry::AgentEntry>(&v)
                                    .map(|e| rz_cli::registry::is_stale(&e, now_ms))
                                    .unwrap_or(true),
                                _ => false,
                            };
                            if legacy || stale {
                                let reason = if legacy { "legacy key" } else { "stale entry" };
                                if yes {
                                    let _ = store.delete(&key).await;
                                    println!("removed KV entry {key} ({reason})");
                                    removed += 1;
                                } else {
                                    println!("would remove KV entry {key} ({reason})");
                                }
                            }
                        }
                    }
                }
                Ok::<(), eyre::Report>(())
            })?;

            if yes {
                eprintln!("gc: removed {removed} objects");
            } else {
                eprintln!("gc: dry run — pass --yes to delete");
            }
        }

        Cmd::Kv(kv_cmd) => {
            if rz_cli::nats_hub::hub_url().is_none() {
                bail!("RZ_HUB must be set for KV operations");
            }
            let client = rz_cli::nats_hub::client()?;
            rz_cli::nats_hub::runtime().block_on(async {
                let js = async_nats::jetstream::new(client.clone());

                match kv_cmd {
                    KvCmd::Get { bucket, key } => {
                        let store = js.get_key_value(&bucket).await
                            .map_err(|e| eyre::eyre!("bucket '{bucket}' not found: {e}"))?;
                        match store.get(&key).await {
                            Ok(Some(bytes)) => {
                                let val = String::from_utf8_lossy(&bytes);
                                println!("{val}");
                            }
                            Ok(None) => {
                                eprintln!("key '{key}' not found in bucket '{bucket}'");
                                std::process::exit(1);
                            }
                            Err(e) => bail!("get failed: {e}"),
                        }
                    }
                    KvCmd::List { bucket } => {
                        let store = js.get_key_value(&bucket).await
                            .map_err(|e| eyre::eyre!("bucket '{bucket}' not found: {e}"))?;
                        let mut keys = store.keys().await
                            .map_err(|e| eyre::eyre!("list keys failed: {e}"))?;
                        use futures::StreamExt;
                        while let Some(key_result) = keys.next().await {
                            match key_result {
                                Ok(k) => println!("{k}"),
                                Err(e) => eprintln!("key error: {e}"),
                            }
                        }
                    }
                    KvCmd::Watch { bucket, key } => {
                        let store = js.get_key_value(&bucket).await
                            .map_err(|e| eyre::eyre!("bucket '{bucket}' not found: {e}"))?;
                        let mut watcher = if let Some(k) = &key {
                            store.watch(k).await
                        } else {
                            store.watch_all().await
                        }.map_err(|e| eyre::eyre!("watch failed: {e}"))?;
                        eprintln!("watching bucket '{bucket}'...");
                        use futures::StreamExt;
                        while let Some(entry) = watcher.next().await {
                            match entry {
                                Ok(entry) => {
                                    let val = String::from_utf8_lossy(&entry.value);
                                    println!("{}: {val}", entry.key);
                                }
                                Err(e) => eprintln!("watch error: {e}"),
                            }
                        }
                    }
                }
                Ok::<(), eyre::Report>(())
            })?;
        }
    }

    Ok(())
}

/// Simple shell escaping for single-quoted strings.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdin_message_keeps_markdown_verbatim() {
        let body = "DONE: split `computational modeling`\n- costs $5\n\n  indented\n";
        assert_eq!(
            stdin_message(body).unwrap(),
            "DONE: split `computational modeling`\n- costs $5\n\n  indented"
        );
    }

    #[test]
    fn stdin_message_rejects_blank() {
        assert!(stdin_message("  \n\n").is_err());
    }

    #[test]
    fn message_argument_wins_over_stdin() {
        assert_eq!(message_text(Some("hi".into())).unwrap(), "hi");
    }
}
