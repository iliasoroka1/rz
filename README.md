# rz

**Universal messaging for AI agents. If it runs in a terminal, it can talk. If it speaks HTTP, it can too.**

Works with any AI coding agent — [Claude Code](https://claude.ai/code), [Gemini CLI](https://github.com/google-gemini/gemini-cli), [OpenCode](https://github.com/opencode-ai/opencode), HTTP APIs, or any process. No SDK, no framework — just a CLI.

- **`rz run`** — start an agent (auto-detects tmux/cmux/zellij, or headless PTY)
- **`rz agent`** — wrap any command in a PTY, works in any terminal
- **`rz bridge`** — connect HTTP services as agents (no rz install needed on their side)
- **`rz send`** — message any agent, anywhere

Connect agents across machines with [NATS](https://nats.io) (`export RZ_HUB=nats://...`).

**Fork of [rz](https://github.com/HodlOg/rz)** by [@HodlOg](https://github.com/HodlOg).

---

## Install

```bash
cargo install rz-agent
```

From source:

```bash
git clone https://github.com/iliasoroka1/rz
cd rz
make install
```

---

## Quick start

### Terminal agents

```bash
# Start NATS (one-time)
nats-server -js
export RZ_HUB=nats://localhost:4222

# Run agents
rz run --name worker claude --dangerously-skip-permissions
rz run --name reviewer gemini

# Send messages
rz send worker "refactor the auth module"
rz ps
```

### HTTP agents

Any HTTP service can be an rz agent through the bridge — no rz install needed on the service side:

```bash
# Start a bridge for your HTTP service
rz bridge --name api-bot --webhook http://localhost:7070/inbox --port 7071
```

The bridge does two things:

**Inbound** — messages from other agents arrive as POST to your webhook:
```json
{"id":"a1b2","from":"worker","text":"process this data","ts":1774568000}
```

**Outbound** — your service sends messages by POSTing to the bridge:
```bash
curl -X POST http://localhost:7071/send \
  -H "Content-Type: application/json" \
  -d '{"to":"worker","text":"here are the results"}'
```

That's it. Your HTTP service is now a full rz agent — discoverable, messageable, and connected to every other agent on the NATS hub.

### Permanent agents

For long-running agents that should survive restarts and receive offline messages:

```bash
rz agent --name server-worker --permanent -- claude --dangerously-skip-permissions
rz bridge --name api-bot --permanent --webhook http://localhost:7070/inbox
```

JetStream stores messages while the agent is offline. On restart with the same name, it picks up where it left off.

### Cross-machine

Agents on different machines, different multiplexers, different languages — all talk through NATS:

```bash
# Machine A (tmux) — terminal agent
export RZ_HUB=nats://nats.example.com:4222
rz run --name worker claude --dangerously-skip-permissions

# Machine B (plain terminal) — another terminal agent
export RZ_HUB=nats://nats.example.com:4222
rz agent --name reviewer -- gemini

# Machine C — HTTP service via bridge
export RZ_HUB=nats://nats.example.com:4222
rz bridge --name api-bot --webhook http://localhost:7070/inbox

# Any machine — send to any agent
rz send worker "implement feature X"
rz send api-bot "process batch 42"
```

---

## How it works

Every message is a single line: `@@RZ:<json>`

```json
{"id":"a1b2","from":"lead","to":"worker","kind":{"kind":"chat","body":{"text":"do X"}},"ts":1774488000}
```

### Registry

Agents register in two places:
- **NATS KV** (`rz-agents` bucket) — global, real-time discovery
- **Local file** (`~/.rz/registry.json`) — fallback when NATS is unavailable

Temporary agents are pruned after 10 minutes of inactivity. Permanent agents (`--permanent`) persist until explicitly removed.

### Message kinds

| Kind | Purpose |
|---|---|
| `chat` | General communication |
| `ping` / `pong` | Liveness check |
| `error` | Error report |
| `timer` | Self-scheduled wakeup |

---

## Backends

| Backend | Command | Best for |
|---|---|---|
| PTY agent | `rz agent --name X -- <cmd>` | Any terminal, SSH, CI, servers |
| HTTP bridge | `rz bridge --name X --webhook <url>` | HTTP APIs, web services |
| tmux | `rz run --name X <cmd>` | tmux users |
| cmux | `rz run --name X <cmd>` | Claude Code desktop app |
| zellij | `rz run --name X <cmd>` | Zellij users |

`rz run` auto-detects the multiplexer. Without one, it falls back to a headless PTY agent.

---

## Commands

### Run agents
| Command | Description |
|---|---|
| `rz run --name X <cmd>` | Start an agent (auto-detects environment) |
| `rz agent --name X -- <cmd>` | Run agent with PTY wrapping |
| `rz bridge --name X --webhook <url>` | Bridge an HTTP service to NATS |
| `rz ps` | List all agents |
| `rz kill <target>` | Stop an agent |

### Messaging
| Command | Description |
|---|---|
| `rz send <target> "msg"` | Send message (auto-routes) |
| `rz broadcast "msg"` | Send to all agents |
| `rz ping <target>` | Check liveness |

### Observe
| Command | Description |
|---|---|
| `rz logs <target>` | Show agent's scrollback |
| `rz log <target>` | Show `@@RZ:` protocol messages only |

### Registry
| Command | Description |
|---|---|
| `rz register --name X --transport T` | Manually register an agent |
| `rz deregister X` | Remove from registry |

Use `rz help <command>` for details. `rz help --all` shows all commands.

---

## Project structure

```
rz/
├── crates/
│   ├── rz-protocol/        # @@RZ: wire format
│   ├── rz-cli/
│   │   ├── main.rs           # CLI + auto-detect backend
│   │   ├── backend.rs        # Backend trait (Cmux, Zellij, Tmux)
│   │   ├── pty.rs            # PTY agent wrapper
│   │   ├── bridge.rs         # HTTP-to-NATS bridge
│   │   ├── tmux.rs           # tmux CLI wrapper
│   │   ├── cmux.rs           # cmux socket client
│   │   ├── zellij.rs         # zellij CLI wrapper
│   │   ├── nats_hub.rs       # NATS (JetStream + core)
│   │   ├── registry.rs       # Local + NATS KV registry
│   │   ├── mailbox.rs        # File-based message store
│   │   ├── bootstrap.rs      # Agent bootstrap message
│   │   └── log.rs            # @@RZ: message extraction
│   └── rz-hub/               # Zellij WASM plugin (optional)
└── Makefile
```

---

## Credits

Forked from [rz](https://github.com/HodlOg/rz) by [@HodlOg](https://github.com/HodlOg).

## License

MIT OR Apache-2.0
