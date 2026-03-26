# rz

**Universal messaging for AI agents — terminal, HTTP, or anywhere.**

rz gives AI agents a native way to find each other and communicate, regardless of where they run. Spawn Claude into a terminal split, register an HTTP agent, or drop messages into a file mailbox — `rz send peer "hello"` works the same way everywhere.

**Fork of [rz](https://github.com/HodlOg/rz)** by [@HodlOg](https://github.com/HodlOg). The `@@RZ:` wire protocol is unchanged from the original — this fork adds transport-agnostic routing, a universal agent registry, and file-based mailboxes alongside the original cmux terminal support.

---

## Why rz

AI coding agents need to talk to each other. A lead agent delegates to coders, reviewers report back, CI bots notify the team. But agents run in different environments — terminals, HTTP servers, IDE extensions, CI pipelines — and can't natively discover or message each other.

rz solves this with one binary:

- **Universal registry.** Agents register once (`rz register --name coder --transport file`). Any agent can find any other with `rz ps`.
- **Pluggable transports.** Messages route through cmux (terminal paste), file mailbox (universal), or HTTP (network agents). The sender doesn't need to know — `rz send coder "do X"` just works.
- **Structured protocol.** Every message is an `@@RZ:` JSON envelope with ID, sender, recipient, threading, and typed payloads (chat, tool calls, delegation, status).
- **File mailbox.** The universal fallback — works everywhere, survives crashes, debuggable with `ls` and `cat`. No daemon needed.
- **Agent spawning.** `rz run claude --name worker -p "do X"` spawns a new agent with identity, peer list, and a task — all in one command.

---

## Install

```bash
# Full version — cmux + file + http + NATS (cross-machine)
cargo install rz-agent

# Terminal-only — cmux only, smaller binary
cargo install rz-cmux
```

Both install a binary called `rz`. Pick one.

Or from source:

```bash
git clone https://github.com/iliasoroka1/rz
cd rz
cargo build --release
cp target/release/rz ~/.local/bin/rz
```

### Crates

| Crate | Description |
|---|---|
| [`rz-agent`](https://crates.io/crates/rz-agent) | Full CLI — all transports (cmux, file, http, NATS) |
| [`rz-agent-protocol`](https://crates.io/crates/rz-agent-protocol) | @@RZ: wire format library (use in your own agents) |
| [`rz-cmux`](https://crates.io/crates/rz-cmux) | Terminal-only CLI (cmux transport only) |
| [`rz-cmux-protocol`](https://crates.io/crates/rz-cmux-protocol) | @@RZ: wire format (cmux version) |

---

## Quick start

### Terminal agents (cmux)

```bash
# Spawn a team
rz run --name lead -p "refactor auth, spawn helpers" claude --dangerously-skip-permissions
rz run --name coder -p "implement session tokens" claude --dangerously-skip-permissions

# Observe
rz list                    # see who's alive
rz log lead                # read lead's messages
rz send lead "wrap up"     # intervene
```

### Any agent (universal)

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

---

## Architecture

```
~/.rz/
  registry.json              # who's alive, how to reach them
  mailboxes/
    <agent-name>/
      inbox/                 # one JSON file per message
        1774488000_a1b2.json
        1774488001_c3d4.json

rz-cmux/
├── crates/
│   ├── rz-protocol/         # @@RZ: wire format (transport-agnostic)
│   │   └── lib.rs           # Envelope, MessageKind, encode/decode
│   └── rz-cli/
│       ├── main.rs          # CLI commands
│       ├── registry.rs      # Agent discovery (~/.rz/registry.json)
│       ├── mailbox.rs       # File-based message store
│       ├── transport.rs     # Pluggable delivery (cmux, file, http)
│       ├── cmux.rs          # cmux socket client
│       ├── bootstrap.rs     # Agent bootstrap message
│       ├── log.rs           # @@RZ: message extraction
│       └── status.rs        # Session status
```

### Transports

| Transport | Delivery method | Best for |
|---|---|---|
| `cmux` | Paste into terminal via cmux socket | Terminal agents (Claude Code) |
| `file` | Write JSON to `~/.rz/mailboxes/<name>/inbox/` | Universal — works everywhere |
| `http` | POST @@RZ: envelope to URL | Network agents (tinyclaw, APIs) |

### Message flow

```
rz send coder "implement auth"
    │
    ├── resolve "coder" → check cmux names → check ~/.rz/registry.json
    │
    ├── transport = cmux?  → paste @@RZ: envelope into terminal
    ├── transport = file?  → write envelope to ~/.rz/mailboxes/coder/inbox/
    └── transport = http?  → POST envelope to registered URL
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

### Discovery & identity
| Command | Description |
|---|---|
| `rz id` | Print this surface's ID |
| `rz list` / `rz ps` | List all surfaces and registered agents |
| `rz status` | Surface counts and message counts |
| `rz register --name X --transport T` | Register agent in universal registry |
| `rz deregister X` | Remove agent from registry |

### Messaging
| Command | Description |
|---|---|
| `rz send <target> "msg"` | Send @@RZ: message (routes via registry) |
| `rz send --ref <id> <target> "msg"` | Reply to specific message (threading) |
| `rz send --wait 30 <target> "msg"` | Send and block for reply |
| `rz ask <target> "msg"` | Shorthand for send + wait |
| `rz broadcast "msg"` | Send to all agents |
| `rz recv <name>` | Read messages from file mailbox |
| `rz recv <name> --one` | Pop oldest message |
| `rz recv <name> --count` | Count pending messages |

### Agent lifecycle
| Command | Description |
|---|---|
| `rz run <cmd> --name X -p "task"` | Spawn agent with bootstrap + task |
| `rz close <target>` / `rz kill` | Close a surface |
| `rz ping <target>` | Check liveness, measure RTT |
| `rz timer 30 "label"` | Self-deliver Timer message after N seconds |

### Workspace
| Command | Description |
|---|---|
| `rz init` | Create shared workspace |
| `rz dir` | Print workspace path |
| `rz workspace create` | New cmux workspace |

### Observation
| Command | Description |
|---|---|
| `rz log <target>` | Show @@RZ: protocol messages |
| `rz dump <target>` | Full terminal scrollback |
| `rz gather <id1> <id2>` | Collect last message from each agent |

### Browser (cmux only)
| Command | Description |
|---|---|
| `rz browser open <url>` | Open browser split |
| `rz browser screenshot <id>` | Take screenshot |
| `rz browser eval <id> "js"` | Run JavaScript |
| `rz browser click <id> "sel"` | Click element |

---

## Credits

Forked from [rz](https://github.com/HodlOg/rz) ([crates.io/crates/rz-cli](https://crates.io/crates/rz-cli)) by [@HodlOg](https://github.com/HodlOg). The `@@RZ:` protocol, bootstrap design, and core messaging architecture are from the original project.

## License

MIT OR Apache-2.0
