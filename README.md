# Trestle (栈架)

English | [简体中文](./README.zh-CN.md)

> Like a craftsperson's trestle holding blueprints, workboards, and verification gauges—supporting task lineage, handoff briefs, and confirmation receipts without intruding on external source repositories.

**Trestle (`wb`)** is a local-first, domain-agnostic personal task, handoff, and delivery-verification workbench written in Rust (`edition = "2024"`).

---

## Philosophy & Architectural Boundaries

- **Single static binary**: Compiles to `wb` (`target/release/wb` or `target/debug/wb`) with zero Python or Node.js runtime dependencies (`Git` is optional; task tracking and the dashboard work even when `git` is unavailable).
- **Strict workspace separation**: The workbench directory stores task metadata, chronological progress, delivery declarations, verification receipts, and design/analysis docs—never copies of external business source trees (`repos_root`).
- **Declarations vs. observations**:
  - `tasks/<task-id>/delivery.yaml` holds explicit delivery targets (`repo`, `worktree`, `source_branch`, `target_branch`, `compare_base`, `allowed_paths`, `depends_on`) and required verification `actions`, protected by optimistic `revision` locking and immutable snapshots in `tasks/<task-id>/delivery-history/`.
  - `.wb-runtime/observations/<task-id>.json` holds read-only Git facts collected by `wb sync` (actual branch, full `HEAD` commit, dirty file count, file SHA-256 digests, branch mismatches, out-of-scope paths). `wb sync` never overwrites `delivery.yaml` or mutates `task.yaml`'s `updated_at`.
- **Append-only progress & receipt-driven confirmation**: `tasks/<task-id>/progress.md` is strictly append-only. Action sign-off requires an explicit receipt (`tasks/<task-id>/evidence/confirm-<action-id>.json`) matching `--by`, `--source`, and `--digest <scope_sha256>`. Completion is never inferred from prose in `progress.md`; any scope change or missing receipt automatically invalidates confirmation.
- **Generated index**: `tasks/INDEX.md` is automatically regenerated on task mutations and workspace upgrades.

---

## Quick Start & Build

### Build & Test (Offline-Capable)

```bash
cargo build --offline
cargo build --release --offline
cargo test --offline
```

Frontend assets for the embedded dashboard (`web/src/{index.html,app.js,styles.css}`) are pre-copied into `src/static/` and compiled into `wb` via `include_str!`. If you edit files under `web/src/`, refresh `src/static/` with:

```bash
cd web && npm run build
```

### Initialize a Workbench

```bash
./target/release/wb init ~/my-workbench --yes --example --agent-guide
cd ~/my-workbench
```

Key `wb init` flags:

- `--dry-run`: Preview directories and files to be created without writing to disk.
- `--name <name>`: Set workbench display name in `.wb.yml`.
- `--repos-root <path>`: External repositories root directory (default `../repos`).
- `--timezone <tz>`: Workbench timezone (e.g., `Asia/Shanghai`).
- `--port <port>`: Default dashboard port in `.wb.yml` (default `8791`).
- `--example`: Generate sample task `TASK-EXAMPLE-001`.
- `--agent-guide`: Create an `AGENTS.md` pointer to `WORKBENCH.md` if absent.

### Workspace Locator Precedence

Every `wb` invocation resolves the workbench root (containing `.wb.yml`) in strict order:

1. Global flag `--workspace <path>`
2. Environment variable `WB_ROOT`
3. Upward directory walk from the current working directory

---

## Core Workflows & `wb` Subcommands

### 1. Task Registration & Second-Level Session Handoff

```bash
# Create a task and register its originating session
wb new TASK-101 \
  --title "Implement cache invalidation strategy" \
  --goal "Prevent stale reads under concurrent writes" \
  --priority P1 --category requirement --owner alice \
  --session-id sess-1 --host devbox --session-title "[FD] Cache design discussion"

# Advance status, append chronological progress, maintain next steps & blockers
wb status TASK-101 in_progress --note "Started core implementation"
wb progress TASK-101 "Completed initial double-delete cache draft"
wb next TASK-101 add "Add concurrent race unit tests"
wb blocker TASK-101 add "Waiting on staging configuration confirmation"

# Generate <= 80-line plain handoff brief for a fresh agent or human session
wb context TASK-101 --plain
```

Additional task management commands:

- `wb context <id> [--plain | --full | --json] [--max-lines 80]`
- `wb next <id> add|done|remove|clear ...` and `wb blocker <id> add|resolve|remove|clear ...`
- `wb milestone <id> add|complete|done|reopen|undone|remove <title> [--done]`
- `wb component|doc|material <id> add|remove ...` and `wb link <id> <url>`
- `wb task list [--status ...] [--category ...] [--priority ...] [--owner ...] [--component ...] [--json]`
- `wb task stats [--json]`
- `wb task record [--task <id>] --session-id <sid> --title "<title>" [--host localhost] [--category ...] [--role associated]`

**Single-prefix session convention**: Session titles may carry at most one bracketed category prefix—`[FD]` (requirement), `[CR]` (code review), `[OC]` (oncall), or `[DA]` (data analysis). Stacked or unknown prefixes are rejected with exit code `2`.

### 2. Delivery Declarations, Read-Only Sync & Action Receipts

```bash
# Apply or update delivery.yaml (pass --expected-revision on updates for optimistic concurrency)
wb delivery apply TASK-101 --file /path/to/delivery-draft.yaml

# Inspect current delivery declaration
wb delivery show TASK-101

# Read-only scan of external Git worktrees (gracefully degrades if Git is absent)
wb sync TASK-101          # or: wb sync --all

# Pin compare_base to the latest observed commit
wb delivery pin TASK-101

# List actions and inspect current scope_sha256 digests
wb action list TASK-101

# Issue explicit confirmation receipt
wb action confirm TASK-101 act-test --by alice --source "local-cargo-test" --digest <scope_sha256>

# Import and export verification artifacts
wb evidence import TASK-101 ./test-report.log
wb evidence export TASK-101 --out ./exported-evidence
```

### 3. Eight-Dimension Code Review Prep, Doctor & Upgrade

```bash
# Generate an 8-dimension engineering review prompt and register 3 aligned output paths
wb review prepare TASK-101 --url https://example.local/pr/42 --report analysis/cr-42.md

# Read-only health check (config, schema, doc refs, branch consistency, invalidated/missing receipts)
wb doctor --json

# Safe workspace template/index upgrade (creates backup before applying)
wb workspace upgrade --dry-run
wb workspace upgrade --apply
```

The 8 built-in review dimensions in `wb review prepare` cover: Scope & Functionality, Root-Cause Effectiveness, Backward Compatibility, Stability & Concurrency, Performance & Cost, Exception Defense, Responsibility Boundaries, and Verification Adequacy.

---

## Read-Only Local Web Dashboard & Extensions

```bash
# Start read-only dashboard bound strictly to 127.0.0.1 (default port 8791 from .wb.yml)
wb dashboard serve --port 8791

# Check running instance status and stop safely (verifies instance_id before shutdown)
wb dashboard status --json
wb dashboard stop

# Inspect discovered extensions and gating/probe states
wb extensions list --json
```

### Dashboard Views & Security Model

The embedded single-page dashboard renders five read-only views (`Overview`, `Task Board`, `Sessions`, `Repos & Reviews`, `Timeline`) using safe `textContent` DOM rendering, plus a slide-over task drawer, Markdown preview modal, and CSV export (`/api/export.csv`).

- **Loopback & origin protection**: Binds exclusively to `127.0.0.1`, validates `Host` (`127.0.0.1` / `localhost`), enforces CSP (`default-src 'self'`), `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, and requires matching `Origin` / `Referer` plus `X-Wb-Csrf-Token` on `POST /api/refresh` and `POST /api/shutdown`.
- **Path & formula guards**: `/api/doc` only reads registered relative `.md` files inside the workspace (blocking `..`, backslashes, symlink escapes, and files `> 2 MiB`) and runs credential redaction (`src/dashboard/redaction.rs`). `/api/export.csv` escapes spreadsheet formula prefixes (`=`, `+`, `-`, `@`, `\t`, `\r`).
- **Bounded collection**: `2 MiB` per file, `10 MiB` per task, `100 MiB` workspace scan cap, and `25 MiB` snapshot JSON cap, retaining the prior snapshot if watched files mutate mid-scan.

### Optional Extensions

Extension manifests (`.json`) discovered under `WB_EXTENSION_MANIFESTS` or `<workspace>/extensions/` remain `disabled` unless explicitly enabled in `.wb.yml` (`extensions: [...]`), then undergo offline local checks (`requires_bins`, `requires_paths`, `probe_cmd`) with per-extension error isolation.

---

## Exit Codes & Workbench Layout

| Exit Code | Meaning |
| --- | --- |
| `0` | Success |
| `1` | Dashboard runtime error (port conflict, `instance_id` mismatch, etc.) |
| `2` | Validation, schema, dependency-cycle, path-escape, or `wb doctor` failure |
| `3` | Target workspace, task, action, or file not found |
| `4` | Optimistic `revision` conflict or `scope_sha256` digest mismatch |
| `5` | Local filesystem I/O failure |

```text
<workspace>/
├── .wb.yml                      # Workbench config (schema, name, repos_root, timezone, dashboard_port, extensions)
├── WORKBENCH.md                 # Collaboration rules written by wb init
├── tasks/
│   ├── INDEX.md                 # Auto-generated task index (do not edit manually)
│   ├── session-types.json       # Deduplicated session category ledger
│   └── <task-id>/
│       ├── task.yaml            # Task metadata, status, milestones, blockers, next_steps, sessions
│       ├── progress.md          # Append-only chronological progress log
│       ├── delivery.yaml        # Optional delivery declaration & action scopes
│       ├── delivery-history/    # Immutable rev-XXXX.yaml snapshots
│       └── evidence/            # confirm-<action-id>.json receipts & imported evidence
├── docs/                        # Long-lived specs & design notes
├── analysis/                    # Investigation & CR review artifacts
├── extensions/                  # Optional extension manifests (*.json)
└── .wb-runtime/                 # Locks, sync observations, upgrade backups, dashboard.json (gitignored)
```

---

## Documentation

- [Design Specification & Data Schema](docs/DESIGN.md)
- [Command Manual](docs/MANUAL.md)

---

## License

MIT (as declared in [Cargo.toml](Cargo.toml))
