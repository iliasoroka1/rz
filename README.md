# rz

**Universal messaging for AI agents — terminal, NATS, HTTP, or anywhere.**

rz gives AI agents a native way to find each other and communicate, regardless of where they run. Spawn Claude into a terminal split, register an HTTP agent, or bridge agents across machines with NATS — `rz send peer "hello"` works the same way everywhere.

**Fork of [rz](https://github.com/HodlOg/rz)** by [@HodlOg](https://github.com/HodlOg). The `@@RZ:` wire protocol is unchanged from the original — this fork adds transport-agnostic routing, a universal agent registry, NATS cross-machine messaging, and file-based mailboxes alongside the original cmux terminal support.

---

## Install

```bash
cargo install rz-agent
```

This installs a binary called `rz`.

From source (macOS requires codesign):

```bash
git clone https://github.com/iliasoroka1/rz
cd rz
make install    # builds, signs, copies to ~/.cargo/bin/
```

### Crates

| Crate | Description |
|---|---|
| [`rz-agent`](https://crates.io/crates/rz-agent) | CLI — all transports (cmux, file, http, NATS) |
| [`rz-agent-protocol`](https://crates.io/crates/rz-agent-protocol) | `@@RZ:` wire format library (use in your own agents) |

---

## Quick start

### Spawn Claude agents

```bash
# Spawn a lead agent with a task
rz run --name lead -p "refactor auth, spawn helpers" claude --dangerously-skip-permissions

# Spawn a worker
rz run --name coder -p "implement session tokens" claude --dangerously-skip-permissions

# Observe and interact
rz list                    # see who's alive
rz log lead                # read lead's messages
rz send lead "wrap up"     # intervene
```

### Universal agents (file mailbox, HTTP)

```bash
# Register agents with different transports
rz register --name worker --transport file
rz register --name api --transport http --endpoint http://localhost:7070

# Send — rz picks the right transport automatically
rz send worker "process this batch"
rz send api "health check"

# Receive from file mailbox
rz recv worker             # print and consume all pending messages
rz recv worker --one       # pop oldest message only
rz recv worker --count     # just show how many are waiting
```

### Cross-machine (NATS)

For agents on different machines, rz uses [NATS](https://nats.io) as a message bus:

```bash
# Start a NATS server (or use a hosted one)
nats-server -js

# Set the hub URL (add to your shell profile)
export RZ_HUB=nats://localhost:4222

# Machine A: listen for messages and deliver to a cmux agent
rz listen worker --deliver "cmux:<surface_id>"

# Machine B: send — routes through NATS automatically
rz send worker "process batch 42"
```

With JetStream enabled (`-js`), messages survive agent restarts and offline periods.

---

## Transports

| Transport | Delivery method | Best for |
|---|---|---|
| `cmux` | Paste into terminal via cmux socket | Terminal agents (Claude Code) |
| `file` | Write JSON to `~/.rz/mailboxes/<name>/inbox/` | Universal — works everywhere |
| `http` | POST `@@RZ:` envelope to URL | Network agents, APIs |
| `nats` | Publish to NATS subject `agent.<name>` | Cross-machine agent coordination |

### Message routing

```
rz send coder "implement auth"
    │
    ├── resolve "coder" → check cmux names → check ~/.rz/registry.json
    │
    ├── cmux?  → paste @@RZ: envelope into terminal
    ├── file?  → write to ~/.rz/mailboxes/coder/inbox/
    ├── http?  → POST to registered URL
    └── nats?  → publish to agent.coder (via RZ_HUB)
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
| `rz close <target>` / `rz kill` | Close a surface |
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
| `rz id` | Print this surface's ID |
| `rz register --name X --transport T` | Register agent in universal registry |
| `rz deregister X` | Remove agent from registry |
| `rz status` | Surface counts and message stats |

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

### Observation
| Command | Description |
|---|---|
| `rz log <target>` | Show `@@RZ:` protocol messages |
| `rz dump <target>` | Full terminal scrollback |
| `rz gather <id1> <id2>` | Collect last message from each agent |

### Workspace
| Command | Description |
|---|---|
| `rz init` | Create shared workspace (`/tmp/rz-cmux-<id>/`) |
| `rz dir` | Print workspace path |

### Browser (cmux)
| Command | Description |
|---|---|
| `rz browser open <url>` | Open browser split |
| `rz browser screenshot <id>` | Take screenshot |
| `rz browser eval <id> "js"` | Run JavaScript |
| `rz browser click <id> "sel"` | Click element |

---

## Project structure

```
rz/
├── crates/
│   ├── rz-protocol/       # @@RZ: wire format (transport-agnostic)
│   │   └── lib.rs          # Envelope, MessageKind, encode/decode
│   └── rz-cli/
│       ├── main.rs          # CLI commands + routing
│       ├── cmux.rs          # cmux socket client (terminal paste)
│       ├── nats_hub.rs      # NATS transport (JetStream + core)
│       ├── registry.rs      # Agent discovery (~/.rz/registry.json)
│       ├── mailbox.rs       # File-based message store
│       ├── transport.rs     # Pluggable delivery abstraction
│       ├── bootstrap.rs     # Agent bootstrap message
│       ├── log.rs           # @@RZ: message extraction
│       └── status.rs        # Session status
└── Makefile                 # build + codesign + install
```

---

## Credits

Forked from [rz](https://github.com/HodlOg/rz) ([crates.io/crates/rz-cli](https://crates.io/crates/rz-cli)) by [@HodlOg](https://github.com/HodlOg). The `@@RZ:` protocol, bootstrap design, and core messaging architecture are from the original project.

## License

MIT OR Apache-2.0
