# Native installation updates

Companion can run and update without a local Constellation Instance. The native
application owns its installation lifecycle. An optional enrolled updater calls
that same lifecycle through command-provider protocol 2 and participant protocol
1; it does not take ownership of Companion's OS service or create another timer.

## First installation

Extract and verify the native package using `docs/native-installation.md`, then
initialize the installation with the package's executable. While the application
is stopped, run:

```text
<package-service> --home <absolute-home> native-update bootstrap --package <absolute-package>
```

This installs the entire immutable manifest bundle and a stable application
entrypoint under the installation's `bin` directory. It preserves configuration,
credentials and conversations. If `companion.json` exists, only its package-owned
Node and bridge paths are rebound. Customized paths are refused, not overwritten.
Startup registration uses that stable entrypoint. Bootstrap does not register
startup, enable voice, enroll an updater or change an update policy. A different
existing executable is preserved and requires an explicit migration.

## Standalone update and rollback

Run the updater from the newly extracted package, not the installed executable it
will replace. This also avoids Windows holding the replacement target open.

```text
<new-package-service> --home <absolute-home> native-update plan --package <absolute-new-package>
<new-package-service> --home <absolute-home> native-update run --package <absolute-new-package>
<package-service> --home <absolute-home> native-update status
<package-service> --home <absolute-home> native-update rollback <completed-transaction-id>
<package-service> --home <absolute-home> native-update recover <incomplete-transaction-id>
```

`recover --rollback` selects the retained previous bundle before restoration has
begun. Once restoration starts, recovery only finishes restoration. A deliberate
rollback of the latest completed update uses `rollback`, which creates a fresh
idle transaction. It does not roll back conversation data, accepted work, outcome
receipts, credentials or user configuration.

For an explicit B-to-A rollback, ordinary recovery continues toward the frozen A
target. `recover --rollback` aborts that incomplete attempt back to the captured
B bundle. If A fails verification, standalone rollback automatically takes this
abort path. B must be verified before admission reopens; if that also fails, the
gate stays closed for recovery. The journal retains the original failure and
records the aborted rollback as `unchanged`, not as successful restoration of A.
An interrupted abort keeps selecting B on subsequent recovery. Neither abort nor
bundle reselection is permitted once restoration has begun.

Preparation atomically closes new call/work admission and checks existing work.
Active calls, accepted background work and unresolved outcomes defer an update;
ending the call alone does not make its background work disposable. A deferred
attempt does not stop the application. An interrupted update retains its journal
and exact transaction identity. It never retries conversation or game effects.

The application proves its identity and settled work through its authenticated
local control endpoint. Manual launches stop through that endpoint; an active
fixed OS startup registration stops and restarts through its original manager.
Verification starts the selected complete bundle under the closed admission gate
and checks installation identity, package identity and the transaction nonce.
On Windows, a retained process handle and creation time establish termination;
a reused numeric PID is insufficient. Originally stopped installations remain
stopped. Existing startup definitions, enabled state and masks are not changed.
Customized or ambiguous registrations defer the update rather than being
rewritten. A plain standalone home does not require an OS service manager; the
absence of one is recorded separately from proof that no registration existed.

The update changes the service executable, private Node, locked bridge dependencies
and support files as one manifest selection. It retains the previous complete
bundle. Failure before restoration permits verified rollback; a lost restoration
acknowledgement permits only idempotent restoration. Database migrations must
remain backward compatible with the retained version; reverting a database
snapshot to discard newly arrived work is never a rollback mechanism.

Mutable lifecycle journals, installation receipts and Windows process-exit
receipts use protected data publication. Existing readers retain their opened
snapshot while new readers see the replacement. Windows readers must permit
delete sharing; denied sharing and read-only destinations still fail. A flush
error after publication leaves the outcome uncertain and requires inspection of
the retained transaction. Executable replacement, directory publication and
create-only ownership records retain their existing rules.

## Optional existing updater enrollment

An installation owner supplies a protected absolute JSON configuration. This is
not accepted from chat or the browser:

```json
{
  "schema": 1,
  "installation_id": "<installation UUID>",
  "home": "<absolute installation home>",
  "component_id": "companion",
  "participant_id": "companion",
  "components": ["companion", "instance", "runtime"],
  "candidate": "<absolute verified candidate package>",
  "helper_package": "<absolute retained immutable helper package>",
  "helper_manifest_sha256": "<exact manifest digest>",
  "helper_executable": "<absolute service executable inside helper package>"
}
```

Choose only the actual enrolled component IDs affecting this installation. The
provider validates the fixed configuration and immutable helper on every call.
The helper cannot be the stable application executable it replaces. Registration
uses fixed arguments and installation-owned paths:

```text
<helper-service> native-update-protocol --config <absolute-config> --mode enrollment --action enroll
<helper-service> native-update-protocol --config <absolute-config> --mode enrollment --action registration
```

The first command records one updater owner per home. The returned registration
delta must be merged by exact component/participant ID using the host updater's
normal controls. The component's generic restart scope is empty:
`{"services":[],"instance":false,"slack_homes":[]}`. Participant registration
lists the affected component IDs; its incoming aggregate scope can include other
enrolled components. Companion alone stops, verifies and restores its application.
Shared-only updates still select the participant when their explicitly enrolled
IDs intersect its configuration.

These commands do not install a scheduler, change hourly/idle policy, discover a
release or download a candidate. Candidate acquisition remains the owner's
existing reviewed package-acquisition path. Plans freeze the selected complete
bundle and owner configuration. Once enrolled, standalone mutation is refused;
use the same enrolled updater for apply and recovery.

The fixed participant CLI uses one monotonic clock for the entire invocation,
including owner validation, OS-manager calls, verification, and result output.
The lifecycle wire remains 30 seconds for prepare/restore and 300 seconds for
verify. Nested waits share the remaining budget: work stops accepting new waits
two seconds before the wire limit, and a process watchdog requests immediate
self-termination with code 124 one second before it. The watchdog stays armed
through JSON serialization and stdout flush, including blocked synchronous COM
calls or output readers. Termination bypasses Rust/C exit handlers and Windows
DLL-detach cleanup; an unexpected failure to terminate aborts the process.
OS scheduling and pending I/O teardown can still delay observed process exit,
so the caller's deadline remains an uncertainty boundary.

Exit 124 means the outcome is **unknown**. It does not prove that an OS-manager
request was cancelled and does not perform compensation or write a terminal
journal receipt. Preserve the transaction, gate and exact process/registration
identity for reconciliation. After restoration has begun, recovery remains
restore-only; no rollback or replay is authorized by a timeout. Ordinary server,
standalone update and direct library calls do not install this participant clock.

## Browser control history and maintenance

Settled native operation registrations are maintained through authenticated
`POST /api/v1/companion/update/cleanup` with an empty JSON object. The request
accepts no caller paths or profiles. It reports confirmed retirement, refusal,
or uncertainty for each retained worker/replacement identity; an active
replacement is preserved. Original launch and retirement evidence remains
private. Explicit package CHECK also performs bounded maintenance, while GET is
read-only. A cleanup inventory refusal is recorded and does not claim cleanup
succeeded or prevent package validation. Uncertain retirement is observation-only
on later requests; it is never automatically dispatched again.

Completed browser operation, request, check, and transaction records are retained
as immutable private UUID files. Their exact bytes are published before active
ledger compaction; old request IDs use direct lookups and keep their original
payload and transaction bindings. Unknown operations remain active. The active
state and each history record have size bounds; completed history does not count
against the active ledger limit. Cleanup reads the same archived authority and
does not modify those records. Outstanding native registrations use a separate
bounded index; confirmed retirement removes index membership while preserving
original launch and retirement evidence.

### CHECK observations

An authenticated browser CHECK retains a bounded, content-free diagnostic
snapshot at `native-browser-update/check-observation.json`. It records fixed
phase names, monotonic elapsed times, blocking-queue delay and the cost of prior
diagnostic writes. It includes an opaque observation ID, process ID and a hash
of the runtime generation when that generation can be read safely. Paths,
credentials, package contents and raw error messages are excluded.

Within a CHECK, each full bundle verification also records four fixed subphases: manifest and
path validation, complete directory inventory, file metadata and content hashes,
and executable entrypoint validation. These retain the same checks and order.
Content verification still uses at most four joined workers. Subphase records
contain no filenames or per-file events. They do not add records or I/O to
standalone bundle verification or first launch. A normal three-scan CHECK fits within
the existing 64-event bound; additional phase sequences retain explicit truncation.
A slow content phase alone does not distinguish file metadata, reads, hashing,
worker imbalance or operating-system inspection costs. A slow inventory phase
does not establish antivirus or filesystem causation.

This snapshot is observational only. `returned` means the synchronous CHECK
function returned; it does not establish that the HTTP reply reached the caller.
`unwound`, missing, stale, truncated or mismatched observations do not prove
cancellation or absence of staging work. No update decision reads this file.
The existing CHECK deadline and no-retry behavior remain unchanged.

Only one CHECK in a process writes observations at a time. Another request runs
under the existing control rules without waiting for diagnostics or replacing
the active trace. Capture errors leave the product result unchanged. An existing
snapshot with unexpected ownership or access policy is left unchanged; capture
is disabled without repairing that policy. File flush latency is not bounded by
the snapshot's size limit, and each snapshot excludes
its initial setup and the duration of its own current write from
`prior_observation_io_ms`. Nested phase durations overlap and can include
diagnostic I/O; they must not be summed. These observations are localization
evidence, not a controlled performance benchmark. Disposable qualification
collects the fixed snapshot with runtime-generation brackets and preserves the
original request error if secondary evidence collection fails.

## Qualification boundaries

The six-target CI matrix exercises package installation with developer tools
absent from the application PATH, actual current-user startup registration, and
disposable whole-bundle lifecycle tests. Test controllers may use Python; the
installed application does not. Fixture candidates with identical service bytes
test bundle selection and lifecycle, not compatibility between arbitrary released
versions. Deliberately incompatible candidates test failure and rollback.

Hosted CI is not a fresh consumer-machine, OS sign-in cycle, microphone/camera,
password-authentication, signing or notarization receipt. Those rows remain
separate until executed on authorized disposable machines. Native packaging and
updater qualification do not activate a paused speech release.
