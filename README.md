# rz

**Universal messaging for AI agents. If it runs in a terminal, it can talk.**

Works with any AI coding agent — [Claude Code](https://claude.ai/code), [Gemini CLI](https://github.com/google-gemini/gemini-cli), [OpenCode](https://github.com/opencode-ai/opencode), or any process that reads terminal input. No SDK, no framework — just a CLI tool that injects messages into terminals.

Three ways to run:
- **`rz run`** — inside [cmux](https://cmux.dev) or [zellij](https://zellij.dev) (spawns panes, auto-detects)
- **`rz agent`** — in any plain terminal (PTY wrapping, no multiplexer needed)
- **NATS** — bridge agents across machines, multiplexers, or both

**Fork of [rz](https://github.com/HodlOg/rz)** by [@HodlOg](https://github.com/HodlOg). Zellij support based on [meepo/rz](https://github.com/meepo/rz).

---

## Install

```bash
cargo install rz-agent
```

From source (macOS requires codesign):

```bash
git clone https://github.com/iliasoroka1/rz
cd rz
make install
```

### Crates

| Crate | Description |
|---|---|
| [`rz-agent`](https://crates.io/crates/rz-agent) | CLI binary — all backends and transports |
| [`rz-agent-protocol`](https://crates.io/crates/rz-agent-protocol) | `@@RZ:` wire format library |
| `rz-hub` | Zellij WASM plugin (build separately) |

---

## Quick start

### Any terminal (no multiplexer)

```bash
# Start NATS (one-time setup)
nats-server -js
export RZ_HUB=nats://localhost:4222    # add to your shell profile

# Terminal 1: run an agent
rz agent --name worker -- claude --dangerously-skip-permissions

# Terminal 2: run another agent
rz agent --name lead -- claude --dangerously-skip-permissions

# Terminal 3: send a message
rz send worker "refactor the auth module"
```

`rz agent` wraps any command in a PTY and subscribes to NATS. Messages arrive as `@@RZ:` lines injected directly into the child's terminal input. Works with any agent that reads from stdin — Claude Code, Gemini CLI, OpenCode, or even plain bash.

```
┌──────────────┐       ┌──────────┐       ┌───────────────────────┐
│ rz send      │──pub──│  NATS    │──sub──│ rz agent --name X     │
│ worker "msg" │       │ agent.X  │       │  └─ injects @@RZ:     │
└──────────────┘       └──────────┘       │     into child's PTY  │
                                          │                       │
                                          │  child = claude, gemini│
                                          │  opencode, bash, ...  │
                                          └───────────────────────┘
```

### Inside a multiplexer (cmux / zellij)

```bash
# Auto-detects cmux or zellij, spawns panes
rz run --name lead -p "refactor auth" claude --dangerously-skip-permissions
rz run --name coder -p "implement tokens" claude --dangerously-skip-permissions

# Observe and interact
rz list                    # see who's alive
rz log lead                # read lead's messages
rz send lead "wrap up"     # intervene
```

### Cross-machine (NATS)

Agents on different machines, in different multiplexers, or in plain terminals — all talk through NATS:

```bash
# Machine A (cmux)
export RZ_HUB=nats://nats.example.com:4222
rz run --name worker claude --dangerously-skip-permissions

# Machine B (plain terminal, no multiplexer)
export RZ_HUB=nats://nats.example.com:4222
rz agent --name reviewer -- claude --dangerously-skip-permissions

# Machine C (zellij)
export RZ_HUB=nats://nats.example.com:4222
rz send worker "implement feature X"
rz send reviewer "review worker's changes"
```

JetStream enabled (`nats-server -js`) gives durable delivery — messages survive agent restarts.

---

## How it works

Every message is a single line: `@@RZ:<json>`

```json
{"id":"a1b2","from":"lead","to":"worker","kind":{"kind":"chat","body":{"text":"do X"}},"ts":1774488000}
```

The `@@RZ:` prefix lets agents distinguish protocol messages from normal terminal output. When an agent receives one, it processes the instruction and replies with `rz send`.

### Message kinds

| Kind | Purpose |
|---|---|
| `chat` | General communication (the only one you need) |
| `ping` / `pong` | Liveness check |
| `error` | Error report |
| `timer` | Self-scheduled wakeup |

---

## Backends

| Backend | How to use | Best for |
|---|---|---|
| PTY agent | `rz agent --name X -- <cmd>` | Any terminal, SSH, CI, remote servers |
| cmux | `rz run --name X <cmd>` (auto-detected) | Claude Code desktop app |
| zellij | `rz run --name X <cmd>` (auto-detected) | Zellij terminal users |

## Transports

| Transport | Delivery | Best for |
|---|---|---|
| `nats` | Publish to NATS subject `agent.<name>` | Cross-machine, cross-backend, PTY agents |
| `cmux` | Paste into cmux surface | Local cmux agents |
| `zellij` | Paste into zellij pane | Local zellij agents |
| `file` | Write to `~/.rz/mailboxes/<name>/inbox/` | Universal fallback |
| `http` | POST to URL | Network agents, APIs |

---

## Commands

### Run agents
| Command | Description |
|---|---|
| `rz agent --name X -- <cmd>` | Run agent in any terminal (PTY + NATS) |
| `rz run <cmd> --name X -p "task"` | Spawn agent in multiplexer pane |
| `rz list` / `rz ps` | List all agents |
| `rz close <target>` / `rz kill` | Close a pane/surface |

### Send messages
| Command | Description |
|---|---|
| `rz send <target> "msg"` | Send message (auto-routes) |
| `rz send --wait 30 <target> "msg"` | Send and wait for reply |
| `rz broadcast "msg"` | Send to all agents |

### Discovery
| Command | Description |
|---|---|
| `rz id` | Print this agent's ID |
| `rz register --name X --transport T` | Register in `~/.rz/registry.json` |
| `rz deregister X` | Remove from registry |
| `rz ping <target>` | Check liveness |

### NATS
| Command | Description |
|---|---|
| `rz listen <name> --deliver <method>` | Subscribe and deliver locally |
| `rz timer 30 "label"` | Schedule a self-wakeup |

Delivery methods: `stdout`, `file`, `cmux:<id>`, `zellij:<pane_id>`

### Observe
| Command | Description |
|---|---|
| `rz log <target>` | Show `@@RZ:` messages |
| `rz dump <target>` | Full scrollback |
| `rz gather <ids...>` | Collect last message from each |

### Workspace
| Command | Description |
|---|---|
| `rz init` | Create shared workspace |
| `rz dir` | Print workspace path |

---

## Project structure

```
rz/
├── crates/
│   ├── rz-protocol/        # @@RZ: wire format
│   ├── rz-cli/
│   │   ├── main.rs           # CLI + auto-detect backend
│   │   ├── backend.rs        # Backend trait (CmuxBackend, ZellijBackend)
│   │   ├── pty.rs            # PTY agent (no multiplexer needed)
│   │   ├── cmux.rs           # cmux socket client
│   │   ├── zellij.rs         # zellij CLI wrapper
│   │   ├── nats_hub.rs       # NATS (JetStream + core)
│   │   ├── registry.rs       # ~/.rz/registry.json
│   │   ├── mailbox.rs        # File-based message store
│   │   ├── bootstrap.rs      # Agent bootstrap message
│   │   └── log.rs            # @@RZ: message extraction
│   └── rz-hub/               # Zellij WASM plugin (optional)
└── Makefile
```

---

## Credits

Forked from [rz](https://github.com/HodlOg/rz) by [@HodlOg](https://github.com/HodlOg). Zellij support based on [meepo/rz](https://github.com/meepo/rz).

## License

MIT OR Apache-2.0
