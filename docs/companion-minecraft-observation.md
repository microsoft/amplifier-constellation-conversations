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

Public replies are associated with the turn's input set, not proof that each
co-admitted request was answered. At most eight whole replies and 8,000 serialized
bytes are returned; additional replies are counted as omitted instead of
truncating a qualification mid-sentence. The v1 proposal does not identify the
recipient and complete co-admitted source set for private replies, so **private
reply content is withheld**, with only a count returned. Possession of the
installation's control token cannot authorize disclosure into a different call.
This consumer support does not establish that a particular installed Minecraft
version advertises the capability; coverage records the actual negotiation.
