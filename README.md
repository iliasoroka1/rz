# rz

**Universal messaging for AI agents — cmux, zellij, NATS, or anywhere.**

One binary that works in both [cmux](https://cmux.dev) and [zellij](https://zellij.dev) terminal multiplexers. Spawn agents, send messages, bridge across machines with NATS — `rz send peer "hello"` works the same way everywhere.

Auto-detects your multiplexer from environment variables (`CMUX_SURFACE_ID` or `ZELLIJ`).

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
make install    # builds, signs, copies to ~/.cargo/bin/
```

### Crates

| Crate | Description |
|---|---|
| [`rz-agent`](https://crates.io/crates/rz-agent) | CLI — cmux + zellij + file + http + NATS |
| [`rz-agent-protocol`](https://crates.io/crates/rz-agent-protocol) | `@@RZ:` wire format library (use in your own agents) |
| `rz-hub` | Zellij WASM plugin for in-process routing (build separately) |

---

## Quick start

Works identically in cmux and zellij:

```bash
# Spawn agents
rz run --name lead -p "refactor auth, spawn helpers" claude --dangerously-skip-permissions
rz run --name coder -p "implement session tokens" claude --dangerously-skip-permissions

# Observe and interact
rz list                    # see who's alive
rz log lead                # read lead's messages
rz send lead "wrap up"     # intervene
```

### Cross-multiplexer (NATS)

Agents in cmux and zellij can talk to each other via NATS:

```bash
# Start a NATS server with JetStream (messages survive restarts)
nats-server -js

# Set the hub URL (add to your shell profile)
export RZ_HUB=nats://localhost:4222

# cmux terminal: spawn an agent — NATS listeners auto-start
rz run --name worker claude --dangerously-skip-permissions

# zellij terminal: send to the cmux agent — routes through NATS
rz send worker "process batch 42"
```

When `RZ_HUB` is set, `rz run` automatically starts background NATS listeners for both the new agent and the lead. No manual `rz listen` needed.

### Universal agents (file mailbox, HTTP)

```bash
# Register agents with different transports
rz register --name worker --transport file
rz register --name api --transport http --endpoint http://localhost:7070

# Send — rz picks the right transport automatically
rz send worker "process this batch"

# Receive from file mailbox
rz recv worker             # print and consume all pending
rz recv worker --one       # pop oldest message
```

---

## Backends

rz auto-detects the terminal multiplexer:

| Backend | Detection | Pane IDs | Best for |
|---|---|---|---|
| cmux | `CMUX_SURFACE_ID` env | UUIDs (`B237E171-...`) | Claude Code desktop app |
| zellij | `ZELLIJ` env | Numeric (`terminal_3`) | Terminal users |

Both backends support the same commands. Backend-specific features:

| Feature | cmux | zellij |
|---|---|---|
| Browser control | `rz browser open/screenshot/eval` | — |
| Pane colors | — | `rz color <pane> --bg #003366` |
| Pane rename | — | `rz rename <pane> "name"` |
| WASM hub plugin | — | `rz-hub` (optional, enables timers + name routing) |

## Transports

| Transport | Delivery method | Best for |
|---|---|---|
| `cmux` | Paste into terminal via cmux socket | cmux agents |
| `zellij` | Paste into pane via zellij CLI | zellij agents |
| `file` | Write JSON to `~/.rz/mailboxes/<name>/inbox/` | Universal fallback |
| `http` | POST `@@RZ:` envelope to URL | Network agents, APIs |
| `nats` | Publish to NATS subject `agent.<name>` | Cross-machine, cross-multiplexer |

### Message routing

```
rz send coder "implement auth"
    |
    +-- resolve "coder" -> names.json -> surface titles -> registry -> NATS
    |
    +-- cmux?    -> paste @@RZ: envelope into cmux surface
    +-- zellij?  -> paste into zellij pane
    +-- file?    -> write to ~/.rz/mailboxes/coder/inbox/
    +-- http?    -> POST to registered URL
    +-- nats?    -> publish to agent.coder (via RZ_HUB)
```

---

## Protocol

Every message is a single line: `@@RZ:<json>`

```json
{
  "id": "a1b20000",
  "from": "lead",
  "to": "coder",
  "ref": "prev-msg-id",
  "kind": { "kind": "chat", "body": { "text": "implement auth" } },
  "ts": 1774488000000
}
```

### Message kinds

| Kind | Body | Purpose |
|---|---|---|
| `chat` | `{text}` | General communication |
| `hello` | `{name, pane_id}` | Agent announcement |
| `ping` / `pong` | — | Liveness check |
| `error` | `{message}` | Error report |
| `timer` | `{label}` | Self-scheduled wakeup |
| `tool_call` | `{name, args, call_id}` | Remote tool invocation |
| `tool_result` | `{call_id, result, is_error}` | Tool response |
| `delegate` | `{task, context}` | Task delegation |
| `status` | `{state, detail}` | Progress update |

---

## Commands

### Agent lifecycle
| Command | Description |
|---|---|
| `rz run <cmd> --name X -p "task"` | Spawn agent with bootstrap + task |
| `rz list` / `rz ps` | List all agents — shows `(me)` next to your own |
| `rz close <target>` / `rz kill` | Close a pane/surface |
| `rz ping <target>` | Check liveness, measure RTT |

### Messaging
| Command | Description |
|---|---|
| `rz send <target> "msg"` | Send `@@RZ:` message (auto-routes) |
| `rz send --ref <id> <target> "msg"` | Reply to specific message (threading) |
| `rz send --wait 30 <target> "msg"` | Send and block for reply |
| `rz ask <target> "msg"` | Shorthand for send + wait |
| `rz broadcast "msg"` | Send to all agents |

### Discovery
| Command | Description |
|---|---|
| `rz id` | Print this pane/surface ID |
| `rz register --name X --transport T` | Register agent in universal registry |
| `rz deregister X` | Remove agent from registry |
| `rz status` | Pane counts and message stats |

### File mailbox
| Command | Description |
|---|---|
| `rz recv <name>` | Read and consume all pending messages |
| `rz recv <name> --one` | Pop oldest message |
| `rz recv <name> --count` | Count pending messages |

### NATS
| Command | Description |
|---|---|
| `rz listen <name> --deliver <method>` | Subscribe to NATS subject, deliver locally |
| `rz timer 30 "label"` | Self-deliver Timer message after N seconds |

Delivery methods: `stdout`, `file`, `cmux:<surface_id>`, `zellij:<pane_id>`

### Observation
| Command | Description |
|---|---|
| `rz log <target>` | Show `@@RZ:` protocol messages |
| `rz dump <target>` | Full terminal scrollback |
| `rz gather <id1> <id2>` | Collect last message from each agent |

### Workspace
| Command | Description |
|---|---|
| `rz init` | Create shared workspace (`/tmp/rz-<session>/`) |
| `rz dir` | Print workspace path |

### Browser (cmux only)
| Command | Description |
|---|---|
| `rz browser open <url>` | Open browser split |
| `rz browser screenshot <id>` | Take screenshot |
| `rz browser eval <id> "js"` | Run JavaScript |
| `rz browser click <id> "sel"` | Click element |

### Zellij-specific
| Command | Description |
|---|---|
| `rz color <pane> --bg #HEX` | Set pane border color |
| `rz rename <pane> "name"` | Set pane title |
| `rz watch <pane>` | Stream pane output in real-time |

---

## Project structure

```
rz/
├── crates/
│   ├── rz-protocol/        # @@RZ: wire format (transport-agnostic)
│   │   └── lib.rs           # Envelope, MessageKind, encode/decode
│   ├── rz-cli/
│   │   ├── main.rs           # CLI commands + auto-detect backend
│   │   ├── backend.rs        # Backend trait + CmuxBackend + ZellijBackend
│   │   ├── cmux.rs           # cmux socket client
│   │   ├── zellij.rs         # zellij CLI wrapper
│   │   ├── nats_hub.rs       # NATS transport (JetStream + core)
│   │   ├── registry.rs       # Agent discovery (~/.rz/registry.json)
│   │   ├── mailbox.rs        # File-based message store
│   │   ├── transport.rs      # Pluggable delivery abstraction
│   │   ├── bootstrap.rs      # Agent bootstrap message
│   │   ├── log.rs            # @@RZ: message extraction
│   │   └── status.rs         # Session status
│   └── rz-hub/               # Zellij WASM plugin (optional)
│       ├── main.rs            # Plugin lifecycle
│       ├── registry.rs        # Agent registry + name index
│       └── router.rs          # Pipe dispatch + message routing
└── Makefile                   # build + codesign + install + publish
```

---

## Credits

Forked from [rz](https://github.com/HodlOg/rz) ([crates.io/crates/rz-cli](https://crates.io/crates/rz-cli)) by [@HodlOg](https://github.com/HodlOg). Zellij support based on work by [@meepo](https://github.com/meepo). The `@@RZ:` protocol, bootstrap design, and core messaging architecture are from the original project.

## License

MIT OR Apache-2.0
