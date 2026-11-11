# API contract

All routes are HTTPS. Use a bearer credential in `Authorization`, or a PAM browser session. Browser mutations require `X-CSRF-Token` from `/auth/session` and a matching Origin. HTTP/2 authority and multiple cookie fields are supported. Bearer credentials are never accepted in query strings.

`POST /auth/login` accepts `{username,password}`. `GET /auth/session` returns `{authenticated,owner,csrf}`. `POST /auth/logout` revokes the current cookie. Each allowed user’s cookie is unique to this installation, Secure, HttpOnly, SameSite=Strict, and expires after eight hours.

Errors use an HTTP status plus `{error}`. Read/write roles are enforced before accessing data. `admin` represents the installation owner and manages endpoint/configuration settings; conversation reads still require membership. Browser members can use `chat` and `observe` operations in their own rooms; the separate provider credential only has `context`, and the source importer only has `import`.

## Conversation interface

All paths below start with `/api/v1`.

| Path | Methods | Behavior |
| --- | --- | --- |
| `/meta` | GET | Installation, endpoint label/command, provider ID, and current version. |
| `/config` | GET, PUT | Full operator configuration. PUT saves for restart; ID and owner cannot change. |
| `/check` | POST | ACP initialization only; no model prompt or session creation. |
| `/conversations` | GET, POST | List accessible conversations with `limit` (max 200), `search`, and `before` cursor. POST `{title,agent_ids?}` creates a private native conversation; omitted agents defaults to `["default"]`. |
| `/conversations/{id}` | GET, PATCH, DELETE | GET includes latest 100 messages, people, agents, owner and shared status. PATCH `{title}` renames native conversations. DELETE removes this stored transcript and source grants, but not the agent's private memory. |
| `/conversations/{id}/messages` | GET, POST | GET accepts `limit` (max 100), `before` sequence cursor, `search`, returning `items,next_cursor`. POST `{text,request_id,target_agent_ids?,attachment_ids?}`; request_id is a UUID. Saves source and recipient delivery rows transactionally, including during active work; returns 202. Capacity rejection returns 429 without accepting the source. A repeated identical request returns 200 without another turn. |
| `/conversations/{id}/cancel` | POST | Explicitly stops this client's active turn, attached subprocess and unsent queued messages. Work already performed is not undone. |
| `/conversations/{id}/deliveries` | GET | Latest 256 per-agent delivery records plus every unresolved row; also included in conversation GET. Requires observe plus current membership. |
| `/conversations/{id}/deliveries/{input}/resolve` | POST | Source author acknowledges unknown/notAccepted with `{disposition:"proceed_without_replay"}` while idle. Requires chat and current membership. Retains source/receipt, never resubmits it, and allows saved newer messages to proceed. |
| `/activity` | GET | Latest 100 turn/tool/context/error records. No view-change acknowledgments. |
| `/events` | GET | SSE `change` notifications, then fetch the needed state. Credentials are rechecked during the stream. |
| `/import-sources` | GET, POST | Owner-only source listing; POST `{source,surface}` creates an independently scoped importer. |
| `/import-sources/{source}/rotate` | POST | Replaces the source credential and enables the source. |
| `/import-sources/{source}` | DELETE | Revokes the source credential immediately; retains imported conversations. |
| `/imports/{source}/{session_id}` | PUT | Append-only transcript snapshots from an authorized external ACP experience. See [transcript imports](#transcript-imports). |

At most one agent prompt per room and four per installation run concurrently. Follow-ups for the currently active agent use negotiated `_session/steering` with `idleBehavior: promptRequired`; ordinary ACP queues them until that prompt finishes. Other recipients retain their own ordered queue. Each ordinary next prompt takes a fair turn among waiting rooms; steering retains the active turn's original deadline. A message is at most 60 KB; pending input is bounded to 32 source messages and 8 MiB including attachments per room. See [input continuity](input-continuity.md) for receipts, unknown outcomes, restored attention, and generic ACP limits.

Relevant ACP progress is exposed as status and activity; private one-to-one replies stream into a persisted message; group drafts are buffered until complete. Unknown-session notifications are ignored. Agents without session loading receive a bounded public transcript (100 messages / 60 KB). A failed attempt to load a saved session retains that binding and reports failure rather than silently replaying work in a new session. This is conversation data, not a new agent persona.

Conversation records include `surface` and `read_only`. Imported records cannot be prompted or renamed through the native web actions. Their source updater can append additional transcript messages.


## Room and endpoint operations

| Path | Methods | Behavior |
| --- | --- | --- |
| `/people` | GET | Allowed local usernames. No host account discovery or creation. |
| `/agents` | GET, POST | All signed-in users see IDs/labels; only the installation owner sees configuration. Owner POST `{id,config}` creates/updates a generic ACP attach command. |
| `/agents/{id}` | DELETE | Owner only; refuses a connection still attached to rooms or the default connection. |
| `/conversations/{id}/members` | GET, POST | Members read participants; room owner adds `{username,share_history:true}`. |
| `/conversations/{id}/members/{username}` | DELETE | Room owner removes a person, revokes room grants and cancels active work. The owner cannot be removed. |
| `/conversations/{id}/agents` | POST | Room owner adds `{agent_id,share_history:true}`. |
| `/conversations/{id}/agents/{agent_id}` | DELETE | Room owner removes this connection from the room. |
| `/conversations/{id}/attachments` | GET, POST | Members list sent files plus their own drafts; POST `{name,mime_type,data}` where `data` is base64. |
| `/conversations/{id}/attachments/{attachment_id}` | GET, DELETE | Authenticated private download, or deletion of the caller's unsent upload. |

Native conversation owners manage titles/membership and deletion. Members can send, stop, search, and download. Unknown or unauthorized room IDs both return 404. Operator configuration requires installation-owner authority. Adding an audience always requires explicit history sharing, revokes prior grants, and discards private working sessions. Imported copies remain owner-private and read-only.

Attachment limits: four per message, images at most 2 MB each, UTF-8 text files at most 200 KB; 64 MB total/20 unsent files per room. The upload route accepts at most 3 MB JSON. Current-turn images are passed only when ACP image capability is advertised; text has an explicit text-block fallback. See [room behavior](shared-rooms.md).

## Import credentials

Create and rotate return `{source,surface,enabled,token,token_file}`. Treat `token` as a secret: it appears only in these authenticated responses, never in source listings or activity. The CLI `import-source create/rotate --token-out <file>` stores it privately instead of printing it. Listings contain only source, surface, enabled and timestamps. Creating an active source returns 409; creating a revoked source with the original surface enables it with a new credential. Source surfaces are immutable. Revoke and rotate do not change imported conversation IDs. Token files are operator copies; revocation is persisted in the database and copying an old file back cannot restore access.

Each token permits only `PUT /imports/{its-source}/{session_id}` with its configured surface. It cannot read any conversation, use the context-provider API, issue prompts or manage sources. Import source names allow letters, digits, hyphens, dots and underscores; all endpoints use trusted HTTPS. The original single-source token migrates once without changing its value or existing history.

### Transcript imports

`PUT /api/v1/imports/{source}/{session_id}` accepts:

```json
{
  "label": "A Toad conversation",
  "surface": "toad",
  "messages": [
    {"id": "user-1", "role": "user", "text": "A question", "created_at": 1793510000000},
    {"id": "agent-1", "role": "assistant", "text": "A reply", "created_at": 1793510001000}
  ]
}
```

Send a complete snapshot with the unchanged existing prefix and any new messages appended. Repeating a snapshot is idempotent. Changing or removing old messages, changing the source surface, or reusing a message ID is rejected. Bounds: 500 messages, 900 KB of text, 1 MB request, 200 KB per message. The caller should verify it is observing the intended agent endpoint before importing. Only user prompts and completed assistant replies should be included; exclude grants, MCP credentials, tool responses, and hidden reasoning. Import never runs a model prompt.

The returned `conversation_id` can be viewed in the web UI and retrieved through the context provider with `surface: "toad"` preserved. Continue the original conversation in its original experience, or start a new web conversation with the same agent. Deleting an imported copy is local removal: a source bridge that still retains and replays its transcript can recreate it.

## View interface

`GET /api/v1/views` lists only the authenticated person’s browser views with state, revision, and last update. A browser retains its view ID locally and displays its short ID in the header. `GET /api/v1/views/{id}` reads it; `PUT` writes:

```json
{
  "expected_revision": 4,
  "state": {
    "selected_conversation_id": "conversation UUID or null",
    "panel": "conversation",
    "draft": "A prepared message",
    "draft_request_id": "",
    "draft_request_sha256": "",
    "search": "",
    "dialog": null,
    "title_draft": "",
    "menu_open": false,
    "details_open": false,
    "participants_open": false,
    "target_agent_ids": [],
    "attachment_ids": [],
    "history_search": "",
    "history_before": null,
    "invite_username": "",
    "invite_agent_id": "",
    "share_history": false,
    "registry_open": false,
    "connection_draft": "",
    "action": {"id": "unique action identifier", "kind": "focus", "target": "composer"}
  }
}
```

Panels: `conversation`, `connection`, `activity`. Dialog: `null` or `rename`. Focus targets: `composer`, `search`, `rename_title`, `history_search`, `invite_username`, `invite_agent_id`. Scroll actions use `{id,kind:"scroll",target:"messages",position:0}`; position `-1` means bottom. Give each focus/scroll action a new ID so the browser executes it once. Other optional view fields default to closed/empty. A revision conflict returns 409; reread before reconciling. Changing a view or draft never sends a message. Sending, cancellation, rename submission, and connection checks remain separate explicit API operations.

The browser persists `draft_request_id` (UUID) and `draft_request_sha256` before transmitting a message. An unchanged draft after a lost HTTP response reuses its source ID; a successful acknowledgment clears these fields. They contain no credentials. Older unresolved messages remain reachable through `window.agentConversationsControl.perform({action:"delivery_show",id})`, which updates the ordinary history view. Delivery recovery is also available through `window.agentConversationsControl.perform({action:"delivery_resolve",id})`; this calls the same authenticated resolution API as the button.

Example voice flow: read the active view, select a conversation and populate `draft`, request focus on the composer, then send through the message API only when authorized. The browser observes the same results through SSE. Rename dialog, its editable title, mobile navigation, and connection-details expansion are also controlled by this view state.

## Context-provider interface

New integrations use `POST /agents/{agent_id}/context/v1/query` with a bearer credential whose sole scope is `context:{agent_id}`. Every operation intersects its existing human/audience authorization with the agent's current `room_agents` membership. Owner-mode queries can see only rooms the configured owner and this agent both participate in. A room grant can authorize the attached agent in another person's private room, but cannot expand beyond that room. Private owner grants still intersect this agent's membership. Removal takes effect immediately for list/read/search/orientation/authorize and previously issued grants.

The original `POST /context/v1/query` route and `context` credential are retained for legacy default-agent continuity, with current default-agent room membership enforced on every operation. A migration assigns existing readonly imports to the default agent without changing conversation IDs, messages, or sessions; future imports receive the same source membership. Legacy/default queries cannot see rooms exclusive to another agent. Do not share this credential with new agents. Scoped tokens cannot call the legacy route, another agent's route, chat, configuration, or administration endpoints. Browser cookies cannot call either source route.

Owner-only management uses `GET`, `POST`, and `DELETE /api/v1/agents/{agent_id}/context-credential`. GET returns metadata without a secret. POST `{}` issues or rotates the single credential for an existing registered agent; optional `expires_in_seconds` is clamped to 60–7,776,000 seconds and defaults to 90 days. DELETE revokes it. Browser mutations require normal CSRF protection. Deleting the registered agent also revokes the credential, even if its ID is later reused. Tokens survive restart and only their hashes are stored by this API.

Issuing returns `{agent_id,scope,provider_id,query_path,query_url,surface,enabled,expires_at,token}`. The token appears once in this authenticated response, never in GET or activity. `provider_id` stays `web:<installation UUID>`, matching existing optional ACP grant metadata. For a client that appends `/context/v1/query`, its provider base URL is `https://<host>:<port>/agents/<agent_id>`. CLI `context-source issue <agent_id> --token-out <file>` saves the token privately and prints only the remaining metadata. `context-source show` and `context-source revoke` use the same API.

Both source routes require a bearer credential even if a valid browser cookie is also present. Query:

```json
{
  "operation": "read",
  "access": {"mode": "owner"},
  "conversation_id": "conversation UUID",
  "thread_id": null,
  "limit": 40
}
```

Alternatively use `access:{mode:"grant",grant:"opaque credential"}`. Source grants are generated for the current room audience during ACP attachment and expire after eight hours. Owner-private grants permit retrieval of rooms the configured owner may access. Shared rooms and other users’ private rooms get room-only grants; their `authorize` response has `owner:false` and `actor_id:room:<id>`. Specifying another conversation with a room grant returns 403. Audience changes revoke outstanding grants; old private working sessions are reset. Delete a conversation to revoke its grants; rotating `secrets/context-token` and restarting revokes the provider credential.

- `authorize` returns `{owner:<scope boolean>,conversation_id,thread_id:null,actor_id,surface,label,observed_at,provider_id}`.
- `list` returns `{items:[{id,conversation_id,thread_id:null,label,surface,url,updated_at,coverage}],complete,observed_at,provider_id}`.
- `read`, `orientation`, and `search` accept optional `before` message-sequence cursor and `query` substring (max 200 bytes). They return `next_cursor` plus `{conversation_id,thread_id:null,label,surface,messages,complete,observed_at,provider_id,url,coverage}`. Each message has `{id,text,actor,role,label,at,status,url,thread_id:null}`.

Reads are bounded to 100 messages / 60 KB, with each message excerpt capped at 20 KB. Oversized messages report `truncated: true` and `original_bytes`, and the overall response reports `complete: false`; nearby short messages remain available. Provenance distinguishes web messages from imported Toad or other source messages. URLs point back to the corresponding owner-authenticated view. A context read cannot send messages or trigger work. The agent service decides when retrieval is appropriate and which audience may receive its resulting answer.

### Explicit action approvals

`GET /api/v1/conversations/{id}` includes `permissions` only for the authenticated person who initiated that turn. Other room members may see that an agent is waiting, but cannot inspect the action arguments or answer its request. Room membership is checked on every read and decision.

`POST /api/v1/conversations/{id}/permissions/{request_id}` accepts `{"option_id":"offered-option-id"}` or `{"option_id":null}` to cancel. It requires `chat` access and normal browser CSRF protection. Only the agent's offered option IDs are accepted, exactly once. Requests expire within three minutes (or at the turn deadline, if sooner). Tool arguments are held in memory, omitted from audit entries, and discarded on cancellation, disconnect or restart. Remembered-policy options are explicitly labelled; the client does not choose them automatically.

The ordinary cancel endpoint sends standard ACP `session/cancel` before closing a connection, with up to three seconds for the agent to acknowledge. Work already performed is not undone. During initialization, cancelling closes the connection before a model prompt is sent. Filesystem and terminal callbacks are disabled by default and can be configured per agent; see [optional local ACP callbacks](client-callbacks.md) for their audience and approval requirements.

View state includes `permission_details`: null or the request UUID whose details should be expanded. A controller can select the room, reveal action details, focus the approval controls with target `approval`, scroll target `permissions`, and use the same authenticated decision endpoint. Private response text continues streaming, tool progress is visible while work runs, and shared-room drafts remain private until complete.

## Structured questions

`GET /api/v1/conversations/{id}` includes `elicitations` only for the authenticated
person who initiated that turn. Each item has `id`, `agent`, `message`, `schema`
and `expires_at`. Another room member cannot inspect or answer it.

`POST /api/v1/conversations/{id}/elicitations/{request_id}` accepts
`{"action":"accept","content":{"field":"answer"}}`, `{"action":"decline"}`
or `{"action":"cancel"}`. It uses ordinary chat authorization and browser CSRF.
Content is validated before submission; decline/cancel discard any supplied
content. Decisions are single-use and expire within five minutes or the active
turn deadline. Agent withdrawal, user cancellation, disconnect and restart remove
unanswered forms. Activity contains decisions, not answers.

The client advertises `elicitation.form` and `elicitation.url`; external steps use
the [sign-in and URL flow](shared-rooms.md#agent-sign-in-and-external-steps).
Forms can arrive during initialize,
new/load setup using the originating `requestId`, or within a bound session using
`sessionId`. Unsupported modes/schemas and foreign scopes are rejected. Flat text,
number, integer, boolean and enum fields (including arrays of choices) are
supported, with bounds and string formats validated. Secret/credential fields and
nested object forms are refused. Answers never grant tool authority.

View state has `elicitation_drafts`, an object keyed by request UUID, holding the
editable field values. Focus target `elicitation` and scroll target `elicitations`
expose the form controls. The packaged UI also exposes
`window.agentConversationsControl.perform({action:"elicitation_draft",id,content})`
and the explicit `elicitation_submit`, `elicitation_decline`, `elicitation_cancel`
actions. Submission can omit `content` to use that view's saved draft. These are
the same private authenticated operations as the buttons; a draft update alone
never sends an answer.
