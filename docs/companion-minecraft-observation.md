# Direct Minecraft observations in Companion

The installed observer reads the connected body's existing presence MCP server
with the official MCP TypeScript SDK. It calls only `minecraft_status` and
`minecraft_recent_activity`, which include this agent's own public speech as
well as other players' public speech. It never calls a model or changes the
world. The incoming-only `/conversation` route keeps its existing meaning.

Install `bridge/minecraft-presence.mjs` beside `minecraft-observe.py`, with the
bridge's locked dependencies. Observer configuration remains installation-owned:

```json
{
  "minecraft_home": "/absolute/path/to/the/game/installation",
  "connection_id": "minecraft-workshop",
  "expected_username": "Coach",
  "node_executable": "/absolute/path/to/node-22/bin/node"
}
```

`node_executable` is optional and defaults to `minecraft_home/bin/node`. It must
be an absolute installed Node 22 or newer executable. Speech, models, event text,
and observer requests cannot choose executable paths, hosts, ports, or tools.
The fixed local port and separate presence token come from the game's installed
configuration. Credentials travel through stdin and authorization headers, never
command arguments or observation output. Missing presence configuration or an
unavailable projection is reported as incomplete public coverage; it is not
replaced with another agent's connection or private state.

`presence_observation` supplies a small current position and nearby-player
projection. Task reports retain their own public or private audience. Public
messages preserve their original source IDs and times. A poll time alone does
not change an event's semantic revision and produce repeated voice updates.
Player positions and activity flags describe observations, not proof of work or
request fulfillment.

Requested status context keeps each included fact with its connection, source
time, and relevant limits. Presence, current assignment state, historical
progress, and public messages remain separate kinds of evidence. A bounded
history does not erase a message it actually contains. Oversized whole reports
are omitted with an explicit count; timestamps and warnings alone must not
become a substitute status answer while the reasoning response is pending.

Private installation-control history is not a general context feed. The observer
emits an operator instruction only when its original UUID is uniquely bound to a
confirmed queue receipt in this call's supplied action batch. A private task
observation likewise requires its original private source to match that batch.
Unknown or duplicate receipts grant no private context. Direct status requests
with no actions do not read `/operator-inputs` and omit private task content;
public speech and public task observations remain available. Companion retains
the caller's own original transcript separately.

The observation cursor advances only for records actually emitted within a
poll's event and byte budget. Records deferred by those budgets remain eligible
on the next poll; a deferred presence snapshot retains its original identity
without being marked consumed. This does not promise recovery of records that
the body's bounded source history no longer retains; coverage gaps stay explicit.

Input handling and task authority are distinct. A confirmed original private
queue receipt can be joined to a source-bound local conversation admission. Its
`conversation` scope reports `admitted`, `settled`, or `interrupted`, with
`admission_layer: local_body`. Admission is not proof of host forwarding. A saved
`last_conversation_turn` can establish local settlement, but does not establish
that a reply answered the request or that the requested work is complete. The
projection says `reply_verified: false`, `task_acceptance_verified: false`, and
`fulfillment: not_established`.

A retained exact private `task_authorizations` record can link a later accepted
amendment to the existing task without replacing its original source. Current
task progress remains an agent report. Missing historical turn evidence stays
unknown; nearby chat, an empty inbox, and the passage of time do not create a
terminal receipt. Durable source-specific reply and terminal receipts require
the Minecraft owner's separately supported outcome projection.

The observer negotiates that optional projection through authenticated
`GET /status`. It uses only an explicit `input_outcomes` capability with
`version: 1`, `method: "GET"`, `path: "/input-outcomes"`, and a numeric
`max_ids` between 1 and 16. It never follows a supplied URL or arbitrary path.
Queries contain only original domain UUIDs from confirmed queue receipts passed
by Companion's call-and-connection-scoped action ledger. With no confirmed
receipt, it performs no outcome lookup. An absent or unsupported capability
retains the legacy evidence above. Advertising support and then returning an
invalid, missing, mismatched or unavailable result leaves the outcome unknown;
the observer does not infer a terminal result from today's task or nearby chat,
and never resubmits an instruction.

Outcome observations have `scope: "conversation"` and
`provenance.kind: "minecraft_input_outcome"`. Their `delivery_state` preserves
the domain's queued/admitted/terminal/interrupted/unknown distinction. A terminal
input maps to `settled`, never task completion. Captured admission time, terminal
time, task identity and blockage remain the original turn's facts. Source-bound
accept/amend authorization receipts are separate from delivery and fulfillment.
Every projection retains `verified: false` and `fulfillment: "not_established"`.
For queued or unknown inputs, `conversation_only` may be `null`: no turn mode was
established. It is not converted to `false`. Admitted, terminal and interrupted
records require an explicit boolean. If supplied, `source_ids` must be a bounded,
unique source set containing the queried ID; it is retained as association
evidence, never permission to look up or act on those other inputs.

Public replies are associated with the turn's input set, not proof that each
co-admitted request was answered. At most eight whole replies and 8,000 serialized
bytes are returned; additional replies are counted as omitted instead of
truncating a qualification mid-sentence. Those observer-local counts remain
separate from the domain's optional omission booleans. A domain
`private_responses_withheld` or `responses_omitted` flag is preserved as
`domain_private_responses_withheld` or `domain_responses_omitted`; it supplies no
count and does not promise that omitted text remains retrievable. An absent flag
stays absent. Malformed flag types invalidate the lookup.

The v1 source set does not establish individual private-response recipients, so
**private reply content remains withheld**. Domain-filtered private text never
arrives at this observer; if a response does arrive marked private, the observer
withholds it and reports only its local count. Possession of the installation's
control token cannot authorize disclosure into a different call.
This consumer support does not establish that a particular installed Minecraft
version advertises the capability; coverage records the actual negotiation.

## Optional installation-scoped human source

Install `minecraft-instruction.py` beside `minecraft-observe.py`; the observer
uses the same strict source-contract validator. This is an installation policy,
not a tool argument or a new ACP field. Configure a dedicated Minecraft caller
credential, distinct from the body, context and presence tokens:

```json
{"version":1,"caller_id":"companion","token_file":"/absolute/private/companion-caller-token"}
```

Pass that fixed file as `minecraft-instruction.py --source-config FILE`; set
`source_config` to the same absolute file in the observer configuration. The
endpoint remains the installed Minecraft home. Both helpers require the exact
v1 contract from `GET /source-capabilities` before using `POST /task` with a
source envelope or `POST /source-outcomes`. The advertised identity and
idempotency semantics, fixed routes, 16-item bound and exclusion of private game
responses must all match. A missing or incompatible capability
is a refusal, not permission to downgrade the request to a broad owner route.

The submitted text is the original retained human wording. Actor, original call,
intent, time, revision and private conversation audience are preserved separately
from the authenticated installation caller. The model's action description and
backend instruction/context wrapper are not substituted for human speech. The
source time is the earliest retained human transcript event in the exact selected
source, with local `time_basis: retained_human_transcript`; a later delegation
does not refresh old speech. Missing historical source mappings retain explicit
legacy timing for legacy connections only. Scoped submission refuses that legacy
time basis, and malformed or changed transcript evidence blocks submission.
The receipt must match the action UUID in `source_request_id` and retains the
distinct Minecraft queue ID. The actor label is the authenticated identifier,
not a model assertion of a Minecraft player identity.
Uncertain submission is not retried automatically, even though the domain offers
idempotency for an explicit identical request.

Follow-through sends the exact original source object with its original queue ID,
including when a later private call requests the outcome. It never replaces the
audience with the later call. Foreign or changed source identity produces no
content. The projection preserves whether a public response belongs to one input
or a shared turn; neither is independently established task fulfillment. Private
game responses and unrelated whispers remain excluded. Legacy receipts retain
their legacy observation path and are never silently promoted to scoped sources.

Validation includes the actual Minecraft body HTTP handler with a disposable bot
and workshop, both helpers, multiple callers, grouped public replies, private
whispers, mismatched original source fields, read-only history preservation and
exact queue/terminal distinctions. This is not proof of a live world action or
that a caller heard a response.

Scoped receipts use `scope: private_installation_input` and
`protocol: installation_inputs.v1`. Their validated outcomes retain
`installation_source`, `domain_response_association` (`shared_turn`,
`single_source`, or `not_established`), and
`private_game_responses_included: false`. These fields remain separate from
local omission counts and domain omission flags. No co-admitted private source
IDs are disclosed. Changed or unauthorized bindings produce an unconfirmed
outcome with no invented actor/audience, admission, or reply. Private response
payloads violate this capability and invalidate the scoped lookup.

Voice context presents recorded public replies before handling metadata. An
explicit `single_source` association describes the domain's link to that input;
`shared_turn` retains the weaker source-set association. Neither promotes the
record to general task fulfillment or changes its `verified: false` value.
Likewise, a confirmed message occurrence can be stated affirmatively without
claiming that unrelated work finished, that the caller heard it, or that the
world is continuously observed.
