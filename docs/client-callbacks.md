# Optional local ACP callbacks

Each registered agent can optionally use this web experience's standard ACP
filesystem and terminal callbacks. Both are disabled by default.
These are tools on the **experience's machine**, not a translation of paths on a
remote agent's machine. Agent-owned tools keep their own policy.

Configure the `client` field within the full agent configuration using the
owner-only `POST /api/v1/agents` operation. For the default connection the same
field is available in the installation config/CLI. The configuration form accepts
the same complete agent object. No special agent implementation or private ACP
extension is required.

Callbacks are served during session creation, restoration and an active prompt
turn, with the same audience policy and one-use approval boundary in each phase.
The first session-bound callback during creation pins the new session ID; the
agent's resulting session ID must match. Filesystem/terminal callbacks during
initialization are refused because there is no session yet. Request-scoped
structured questions during initialization use the separate form flow.

Example (replace the workspace, room UUID, usernames and agent IDs with the exact
intended audience):

```json
{
  "read_text_file": true,
  "write_text_file": true,
  "terminal": false,
  "workspace_roots": ["/home/alice/projects/reviewed-work"],
  "allowed_rooms": ["12345678-1234-4234-8234-123456789abc"],
  "allowed_users": ["alice", "robin"],
  "allowed_agents": ["default", "reviewer"],
  "permission_timeout_seconds": 180,
  "max_file_bytes": 1048576,
  "terminal_output_bytes": 262144,
  "terminal_timeout_seconds": 120
}
```

The room must be listed, the initiating person must still be a member and every
human **and agent** in that room must appear in the allowlists. Adding a new
participant removes these capabilities from the next handshake until the service
owner expands the policy. Room membership changes cancel active work. A private
workspace is never implicitly granted to another room or another human. Connection
probes advertise no callbacks because they have no conversation audience.

The agent working directory must exist locally inside an allowed root. Dedicated
workspace roots cannot contain the service installation, service credentials,
the user's whole home directory or system directories. On Unix, each root is
opened once without following symlinks, then retained by its directory descriptor.
Reads and writes walk relative to that descriptor and reject symlinks, hard-linked
files, devices, FIFOs, non-regular files and path traversal. Replacing a root path
with a symlink cannot redirect an approved operation. Windows callbacks remain
unavailable; no unconstrained fallback is used.

## Human decisions and receipts

Every `fs/read_text_file`, `fs/write_text_file` and `terminal/create` callback uses
the existing permission UI/API. The request is visible only to the person who
initiated the turn. It shows the actual operation and parameters. Large writes
show a clearly labeled preview, exact byte count and content hash. Approval binds
the **entire** content, not only the preview.

Only that explicit `Allow once` decision produces an in-memory receipt. It binds
the method, complete parameters, session and this specific turn, is consumed by
value, and expires after 30 seconds if not used. A separate generic permission
request cannot authorize a later callback. Denial, timeout, cancellation, browser
absence, session mismatch or restart grants nothing. `Allow always` is never
substituted for this callback decision.

Files are bounded by `max_file_bytes` (at most 2 MiB). Text reads support ACP's
one-based `line` and `limit` fields. Parent directories for a newly approved write
can be created only inside the retained root. Callback activity records method,
agent and outcome; file bodies and command arguments are not put in activity.

## Isolated terminals

Terminal callbacks currently require Linux and `/usr/bin/bwrap` with working user
namespaces and descriptor mounts (`--bind-fd`/`--ro-bind-fd`). A confined preflight
must succeed **before** `terminal: true` is advertised. The adapter does not change
AppArmor or host policy. macOS/Windows terminals remain unavailable.

The process receives only read-only system binaries/libraries, a fresh `/tmp`,
a private PID namespace and the explicitly chosen workspace roots. Host `/etc`,
other home directories, service credentials, inherited environment credentials
and host networking are not supplied. Workspace mounts are read-only unless the
operator also enables `write_text_file`; an explicit terminal approval then covers
that command's workspace writes. No networking opt-in is implemented in this
version. Bubblewrap receives the pinned root descriptors, not re-resolved paths.

Eight terminals at most can be live for one ACP turn. Captured output is bounded
(at most 1 MiB) and reports truncation. `terminal/output`, `wait_for_exit`, `kill`
and `release` are restricted to terminal IDs created in that same session/turn;
they need no second approval for the already-approved process. Waiting does not
block a concurrent kill request. Timeout, turn end, cancellation or disconnect
kills the process group and its isolated descendants. There is no background
survival across turns.

## Validation

The ordinary Rust suite covers disabled defaults and old config migration,
room/person/agent audience checks, exact receipt matching, changed payloads,
foreign sessions/turns, expiry, file bounds, private installation boundaries,
symlink/hard-link/FIFO refusal and a pinned root replaced after opening.

Integration tests run a real independent ACP process through the driver:
write/read callbacks wait for the correct human, another human cannot approve,
cancellation removes pending approval without writing, and an unlisted room
member removes capabilities from the actual initialize handshake. Creation/load
callbacks and cancellation during creation use the same checks. No model or
external service is used.

On a prepared Linux host, also run:

```sh
cargo test --lib client_callbacks -- --ignored
```

This explicit test executes real bubblewrap, proves the service directory and
host `/etc` are absent, checks that outbound networking is unavailable, checks
output truncation and verifies that timeouts kill a child before it can write a
late file. It is intentionally not treated as a macOS or Windows terminal test.
