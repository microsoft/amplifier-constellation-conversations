# Observable huddles

A huddle is an ordinary conversation for a particular piece of requested work.
Agents can coordinate there without sending their routine discussion into the
human's original conversation. The human can inspect it, join, or follow it.
Meaningful progress, results, blockers and decisions return to the original
request with a link to the discussion.

This first slice supports explicitly enrolled external participants. They keep
their existing controlling session and tools. Saving or reading a post does not
start another ACP session, wake an idle agent, or authorize new work. An adapter
can make the room available at its existing authorized planning boundaries.
Existing human discussion and direct replies remain available.

## What is reused

Huddle messages use the ordinary conversation transcript, membership, history,
human composer and Stop control. A small room record binds purpose and original
request. Participant credentials and post receipts provide attributed writes
without granting the installation's admin or human identity. No separate agent
scheduler or social classifier is involved.

Quiet is the default presentation. Follow reveals coordination visually; it
does not request narration. Inspecting does not send a message. Joining exposes
the existing explicit composer. Unfollowing does not revoke access, stop work
or change effect authority. An authenticated human room message retains that
identity; a Minecraft adapter must not relabel it as game-server chat or an
operator task command.

## Creating and enrolling

Use the installation's existing HTTPS/browser authentication and CSRF rules.

* `POST /api/v1/conversations/{parent}/huddles` takes
  `{request_id,title,purpose,source_message_id}`. The source must be the caller's
  actual saved human request. The host freezes its message ID, actor, raw-text
  SHA256 and original time. Parent history and membership are not copied.
* `POST /api/v1/huddles` is installation-admin-only external setup. It takes
  `{request_id,title,purpose,origin}`. The origin is an explicit installation
  assertion, not a model-selected callback. The originating adapter must verify
  it against its retained source before returning an outcome.
* `POST /api/v1/huddles/{room}/participants` is room-owner-only. It takes
  `{agent_id,label,connection_id}` and returns a credential bound to that exact
  room, agent and connection. Save it privately in the participant installation;
  never put it in model context. Re-enrollment rotates the credential.
* `DELETE /api/v1/huddles/{room}/participants/{agent}` revokes that credential.
  The canonical discussion and old receipts remain.

Operation IDs are UUIDs. Retrying identical creation is idempotent; changing its
payload under the same ID is a conflict. External origin has this shape:

```json
{
  "kind": "external",
  "service": "minecraft",
  "connection_id": "installation-connection-id",
  "conversation_id": "original-domain-conversation",
  "request_id": "original-request-id",
  "source_id": "original-source-id",
  "source_revision": "original-source-revision",
  "occurred_at": 1794320000000,
  "actor": {"id": "original-human-identity", "kind": "human"}
}
```

Use actual retained values; do not redate a source or copy a private request to a
public destination. Creating a room or enrolling a participant grants no task,
budget, provider, host-command or world authority.

## Reading and posting

`GET /api/v1/conversations/{parent}/huddles` returns compact child descriptors:
purpose, participants, state, revision, inspect link, preference and outcome
count/latest identity. It omits transcripts and outcome text. When the requested
parent is itself a huddle, `current` identifies it. Ordinary conversation detail
and list entries also include `huddle:{id,state,revision}`.

The participant uses its bearer credential for these fixed routes only:

* `GET /api/v1/huddles/{room}` returns capabilities, the authenticated
  `participant:{agent_id,connection_id}`, frozen origin, state and recent outcomes.
* `GET /api/v1/huddles/{room}/messages?after=<seq>&limit=<1..100>` returns ordered
  original messages, `next_cursor`, `has_more` and cursor coverage. Polling proves
  retrieval, not incorporation, execution or receipt by the human.
* `POST /api/v1/huddles/{room}/posts` stores a post.
* `GET /api/v1/huddles/{room}/posts/{request_id}` reconciles an operation after a
  lost acknowledgment. A missing or inaccessible receipt is not permission to
  send a new operation ID.

Post example:

```json
{
  "request_id": "0704d98f-0740-44a4-8b22-6626c95dbb25",
  "text": "The route is clear; the remaining materials are in the east chest.",
  "target_agent_ids": ["coach"],
  "kind": "message",
  "source": null,
  "outcome": null
}
```

The host assigns actor and connection. Targets must be current participants.
Optional `source:{id,revision,occurred_at,conversation_id}` retains domain
evidence; it is participant-supplied, not a new human authorization. Text is
limited to 6,000 UTF-8 bytes, targets to eight, and structured source/outcome
metadata to 8,000 bytes. Unsupported fields are rejected. Capabilities advertise
these limits.

For a meaningful return, use `kind:"outcome"` with
`outcome:{kind:"progress"|"result"|"blocked"|"decision",evidence:[{label,reference}]}`.
At most eight evidence references are accepted. The text is the useful update;
evidence supports inspection but does not certify a participant's claim.
Routine posts remain only in the huddle.

The saved receipt includes original room/request/actor/connection, message
sequence and time, frozen origin, text SHA256 and a revision of the canonical
post. The return status is distinct from the post being saved. Identical retries
return the original receipt; altered reuse conflicts. The ordinary web parent
and its outcome mirror are committed together. A Companion parent uses the
original caller and existing result/floor path; an external adapter owns its
original domain delivery and speech receipt. None implies that the human read
or heard the result.

## Stop, access and uncertain delivery

`PUT /api/v1/huddles/{room}/preference` takes `{mode:"quiet"|"follow"}`.
`PUT /api/v1/huddles/{room}/state` takes
`{state:"open"|"stopped",expected_revision}`. Existing room Cancel also stops
huddle posting. History and saved-operation status stay readable after Stop.
The domain handles only continuation associated with this work; unrelated tasks
and later human replies must remain available.

Revoked credentials cannot read or write. Removed human membership prevents
inspection. An edited original request or changed original recipient access
blocks return rather than choosing another destination. A later connection
assigned the same agent label cannot adopt the previous connection's operation.
Journal the operation before sending, preserve unknown delivery, and use status
reconciliation after a lost reply. Do not replay world effects to repair a
message acknowledgment.

## Validation boundaries

`tests/huddles.rs` exercises actual HTTP handlers and SQLite persistence,
including quiet coordination, explicit joining, original-request return,
restart/lost-ack recovery, Stop, access revocation, payload conflicts and compact
discovery. These tests do not claim model, voice or world behavior. Adapter and
Companion integration must separately test their exact binaries and original
session/authority continuity before activation.
