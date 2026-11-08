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
