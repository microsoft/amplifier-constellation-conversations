# Constellation Companion (preview)

Companion is a foreground web voice experience for an existing ACP agent. Open
`/companion`, choose a saved contact, and start a call. One person calls one agent
at a time. It is the same agent identity, with a new private conversation and
backend session for each call. A call does not replace its Slack, Minecraft, or
another person's session.

OpenAI GPT-Live (`gpt-live-1`) handles the spoken conversation through client
delegation. The selected agent handles work through Conversations' existing
ordinary ACP connection. Its runtime and model remain selected by its own
installation. Even a local-model agent uses cloud audio and selected text context
for this voice provider; this is not a fully local call.

## Using a call

Microphone access starts only after **Call**. Camera access is optional and starts
only after **Camera**. **Capture & share** immediately uploads one photo and queues
an availability notice for voice. It does not require a typed question or force an
automatic summary. The live camera preview stays on the device.

Drop files or paste text and images into **Shared space**, or choose **Share files**.
Images and PDFs can be up to 2 MB each; UTF-8 text, Markdown, CSV, and JSON can be up
to 200 KB each. Share up to four files at a time. These inputs use the same saved
attachment path as camera captures. The ongoing conversation determines whether
the agent inspects an item or asks a useful follow-up question.

Treat the current shared space as a small whiteboard. Prefer one useful item;
use up to three when a comparison needs them. The agent can add, replace, remove,
or clear current items through the existing view tool. Results and observations
stay in collapsed history unless deliberately presented. Clearing the board does
not delete saved material, cancel work, or imply that anyone saw it. Pinned items
and an item the caller is reading are protected; an incompatible change fails
without partially rearranging the board. The caller can unpin or move an item to
history. A saved presentation request is not confirmation that it is on screen.

An image explicitly attached by the agent to a completed answer in the current
call also requests whiteboard space. This requires an exact link to the caller's
request and the same agent, connection, and private audience. A newer distinct
request can supersede that presentation; a listening acknowledgment or slow
response alone does not. Unlinked or superseded images remain in history. The
request still respects pinned items, the item being read, and the three-item
limit; receipt of an image does not prove it was visible. Native image attachments
are published with the completed answer. An empty board before that answer is
not evidence that attachment publication is unavailable.

Changes to the authenticated browser's current board are also supplied as concise
application context. Repeated presence heartbeats do not create repeated notices;
pending membership updates coalesce to the latest board and wait for the same
conversational floor as other context. Card identities link this observation to
the saved answer. Browser-reported membership does not establish that the person
looked at, decoded, or understood an image. It does establish that the matching
item is currently on the board, superseding the earlier publication-time
uncertainty. An empty-board observation establishes absence from the current
board, not deletion from history.

Publication identifiers and screen-state facts are supplied as quiet context.
Spoken updates contain concise, speakable results; application instructions and
receipt-processing details are not sent as commentary. The original backend
answer stays intact in history. These channel choices guide the voice model,
but do not guarantee its wording: qualification includes recorded playback as
well as provider transcripts and browser display evidence.

An explicit **Paste** control opens a text composer; pasted images use the saved
upload path. A receipt says when material is available. New content follows the
bottom only while the caller is already near the bottom; reading older content
preserves their place. The same rule applies to captions and the transcript.

The shared pane and transcript scroll independently of the call controls, on
desktop and mobile. Conversation and work-control links reuse a separate work tab
so the call can remain open. Answer links are clickable. Supported saved media
can appear inline; remote HTTP(S) media previews require a click. An ordinary
page or repository link remains a link, and a displayed video does not prove it
was played or watched. Answer text is never executed as HTML or JavaScript.

### Work huddles

An installation can link an enrolled coordination room to an actual saved human
request. **Work huddles** then appears in that conversation or call. **Inspect**
opens its attributed discussion inside Companion without ending the call or
replacing anything on the whiteboard. **Join discussion** opens a message box;
only **Send message** posts, using the signed-in person's existing room access.
It neither grants game authority nor creates another agent session.

Huddles start quiet. **Refresh discussion** reads a new page on request;
**Follow details** also refreshes the inspected discussion during ordinary UI
updates. Following affects this person's presentation, not membership, work
authority, or whether a voice update is spoken. Closing the panel or returning
to quiet keeps accepted work intact. The full conversation's **Stop huddle**
control is a separate action. Stopped history and uncertain-message status remain
inspectable. Huddle messages support links to evidence, not file attachments.

Routine peer messages stay in the huddle. Authored outcome posts can return to
the original request as attributed reports, preserving their evidence and time.
Private Companion returns revalidate the original caller, request, source and
agent endpoint. They use existing call cards and the conversational floor, with
no automatic replay of a dispatched or uncertain voice update. A long report
stays complete in saved history; the voice context points to it rather than
clipping an assertion away from its qualification. A saved or spoken report is
not independent proof of its claims or proof that the caller heard it.

An uncertain human send retains its exact source ID in an existing server-side
view. **Check message status** reads for a matching saved message; it does not
resend. Browser storage keeps only that view's opaque ID. If the call's existing
history capacity prevents a return card, the canonical huddle result remains
saved with an explicit blocked presentation receipt.

With installation-owned huddle connections configured, the agent can discover
approved teammates and create or reuse a room through its existing Companion
MCP tools. The caller does not prepare rooms or enroll individual participants.
When an agent has more than one connection, discovery supplies distinct
installation IDs; the tool never chooses a world from the agent's display name.
The exact current delegation selects the saved human request. The host supplies
its actor, original time and revision and rechecks authority when storing the
operation. Neither a room nor a post receipt proves a teammate noticed or
accepted the work. Invited peers use their existing connection's delivery loop;
Companion creates no additional controlling agent session.

The tools are `companion_huddles` (installation choices, roster and room pages),
`companion_huddle_ensure`, `companion_huddle_read`, `companion_huddle_post` and
`companion_huddle_status`. Routine posts stay in the room; meaningful outcome
posts use the existing original-request return. After an uncertain ensure,
inspect its original request ID even if no room ID was received. Saved ensure
and post receipts remain readable after the call ends; new writes do not.
Stopped or revoked rooms are not silently recreated or reopened.

Discovery and read tools may show authorized rooms from earlier calls or other
connections. A Companion call can post only into a room whose original request
belongs to that same private call conversation and caller. A later delegation
within the call can contribute, preserving its own source while the room's
original request and return destination stay unchanged. Rooms anchored elsewhere
remain readable; a new call uses its own source-bound huddle rather than sending
a new outcome to an old conversation. The server repeats this destination check
in the post transaction alongside the current request's authority checks.

An explicitly targeted peer reply can quietly continue the local lead's existing
native session after its initial request turn finishes. The host requires a
prior directed request, the original caller's retained authority, current room
membership and the same call, endpoint and session. It stores the peer's actual
post and author separately from the original human request; it creates no human
message or new delegation. Broadcasts and membership alone do not schedule work.

This continuation may inspect existing context and discuss the original request.
It cannot create new huddles, start domain actions, change the whiteboard or end
the call. Routine final text remains quiet; an explicit outcome uses the normal
return path. A successfully incorporated human correction retains the ordinary
human-authorized path; a queued, rejected or unknown correction grants nothing.
Unknown delivery is never retried automatically. Later same-room human requests
keep their own source records; a peer post without `source` does not establish
which later message it answers.

Installation setup is described in [Observable huddles](huddles.md). It does not
schedule recurring coordination. Owners keep participant credentials in their
self-contained installations; callers are not asked to manage those credentials.

### Voice preferences

Choose **Voice** on a contact, or **Voice settings · next call** during a call.
Preferences are saved for the signed-in person and selected agent. A change takes
effect on their next call; it does not change the current voice, ACP identity,
model, or working session.

The settings panel also identifies the configured cloud voice model and the
reasoning model/settings last reported by the selected ACP backend. Unknown
settings remain unknown. Only advertised model and thinking selections are
projected, with an observation time; launcher commands, credentials, and private
configuration are not exposed. Backend values are read-only here. They are not a
claim that a setting from an earlier call is still current.

Hanging up releases microphone, camera, audio, and the call's optional tools. It
closes the voice connection but does not cancel already submitted connector work
or accepted ACP work. Use the linked conversation and the connector's controls to
review or stop that work. Muting changes only microphone input; interrupting
speech does not retract an operation.

## Installation

Use a separate Conversations installation for the preview. Existing text rooms
and browser dictation remain available. Build Conversations normally, and install
the bridge using Node 22.6 or later:

```sh
cd bridge
npm ci --ignore-scripts
npm test
```

Keep the bridge and its locked dependencies in an installation-owned directory.
Set `OPENAI_API_KEY` in the service environment, preferably through a private
credential file. It is not sent to the browser or copied into the ACP agent's
environment. The official OpenAI SDK owns provider transport. The Rust client uses
the official ACP SDK through `amplifier-acp-connection`; Companion adds no ACP wire
methods.

Register existing ACP endpoints through Conversations' saved-agent API. Remote
agents may use fixed SSH stdio attach commands, with credentials kept on their
own host. Add `companion.json` to this installation's home. Paths below are examples;
use installed paths and actual saved agent IDs:

```json
{
  "enabled": true,
  "node": "/opt/companion/bin/node",
  "bridge": "/opt/companion/bridge/live.mjs",
  "max_call_seconds": null,
  "profiles": {
    "coach": {
      "voice": "marin",
      "description": "Native reasoning · OpenAI cloud voice",
      "identity": "Coach, an existing agent with connected experiences.",
      "observed_at": "2026-11-07T00:00:00Z",
      "connections": [
        {
          "name": "Minecraft",
          "description": "An existing connected experience. Consult the agent for current activity and available controls."
        }
      ],
      "context_registered": false
    }
  }
}
```

`identity`, `observed_at`, and `connections` supply a bounded installation snapshot
at call startup. Update them when the installation changes. The timestamp states
when those facts were observed; the snapshot does not prove current activity,
current permissions, or live tool availability. Missing connections are unknown.
Voice is instructed to consult the existing agent before denying uncertain
knowledge, connections, or capabilities. Starting a call does not itself prompt
the backend model to retrieve live knowledge.

`context_registered` stays false until this installation's optional transcript
provider has actually been registered and made reachable by the agent. With false,
call transcripts remain local and relevant speech accompanies explicit delegation
over ordinary ACP. The agent retains access to already authorized sources. This
flag says nothing about whether its other Slack or Minecraft context is available.
Automatic subscriptions to activity in other experiences are not enabled here.

Serve behind trusted HTTPS, including on phones. The host's normal PAM login
protects browser access. Opening the page does not grant microphone consent, and
the signed-in person authorizes the microphone channel without speaker identity
being inferred from voice. Existing agent permissions and work controls still
apply. Screen locking or switching apps may interrupt audio; background operation
is not guaranteed. Silent or muted provider sessions may still incur charges.
`max_call_seconds` is an optional positive integer. Omit it or set it to `null`
for no local duration cutoff; an explicit value still closes the call when that
many seconds have elapsed, without the former 30-minute ceiling. Browser heartbeat
loss, revoked access, Stop/hangup, and actual provider closure still end calls.
[GPT-Live manages long conversations](https://developers.openai.com/api/docs/guides/live-conversations)
automatically; removing the local cutoff does not override provider limits or
turn a provider `expired` event into a successful ongoing session.

A rejected request with an empty, unbound pre-submission delivery is reported as
not started. The original source remains saved and consumed, and is not retried.
Submitted, bound, or receipted interruptions retain their uncertain outcome;
failing to start a new request says nothing about earlier work.

## Sharing, capabilities, and receipts

Sharing reports separate facts:

- **Captured/selected:** the item exists locally; bytes are not yet saved.
- **Uploading:** transfer is in progress.
- **Available:** the service confirmed saved bytes in this private conversation.
- **Voice notification queued:** the host accepted an awareness notice. Provider
  acknowledgment, speech, playback, and inspection are separate facts.
- **Failed or unconfirmed:** availability could not be established. An uncertain
  upload may have saved bytes; inspect the conversation before sharing again.

Saved items can accompany a subsequent delegation through the existing attachment
path. No generic photo question or independent summary message is sent.
A lost awareness acknowledgment can be checked using the same upload identity.
An unknown storage outcome is never automatically uploaded again.

The UI probes the selected endpoint's ACP initialization capabilities. Images may
still be saved when image inspection is unavailable or unverified. GPT-Live does
not receive the camera feed or inspect the image directly. PDF inspection depends
on the endpoint's embedded-document support; text has a readable fallback. Saving
an artifact never licenses a claim that it has been seen or read.

For the native Constellation runtime, `provider.native.image_input` controls the
enabled image path. An omitted or disabled setting can make an image-capable model
advertise `image: false`; that flag alone does not establish the model's abilities.
The voice starts with image inspection support **unknown**. Its current-call
capability is learned from the actual selected backend's ACP initialization. The
raw observation is retained even if session setup fails. After successful setup,
it is supplied once per observed revision as factual thinking context through the
normal delivery queue, not as a speech-interrupting instruction. A previous
call's settings, static profile, camera permission, or installed tools do not establish support for this call.
An explicit `image: false` means the voice must not promise photo inspection;
files can still be saved or displayed. `image: true` enables the inspection path,
but does not prove that any particular image was delivered or understood.

The optional `image_input` in `companion.json` is informational, not an override of
ACP capabilities. Enable a supported path in the agent installation, reconnect,
then verify fresh initialization and an actual image prompt. Preserve the selected
model and credentials; changing the model is not implied. There is no automatic
cloud vision fallback for a local-model agent.

## Work, results, and uncertainty

**Connected** means audio and control are established, not that backend work has
been accepted. **Delegated** means a timed provider request was associated with the
authenticated caller's transcript and submitted through the ordinary ACP delivery
path. Transcript events are fragments, not complete-turn markers, and delegation
events contain an ID and offset rather than a task description. Missing or
ambiguous requests are held instead of invented. Durable identities prevent
repeating accepted submissions; unknown admission is never automatically replayed.

Settled public results retain their request identity and full text in the
conversation. The card projection is bounded; **Show full result** loads the complete saved
answer through its authenticated result URL. A quiet status field reports pending
work or a required decision without adding a chat message. Short current results can enter spoken context directly. Longer
results use bounded sentence or paragraph fragments, retaining qualifiers within
each included fragment and explicitly reporting omissions. Voice is instructed
not to fill in missing material. Delayed results are marked as answers to an
earlier request and enter quiet context rather than an instruction to interrupt
the current topic. Generated text and provider context acknowledgments are not
proof that audio played or the caller heard it.

## Optional connected-experience tools

An installation can opt into tools by adding `companion-actions.json` to its home.
Each configured connection maps to a fixed executable and arguments chosen by the
installation owner. No tool argument selects a host command, credential, arbitrary
filesystem path, or callback origin. For example, use the bundled helpers with installation-owned configuration:

```json
{
  "enabled": true,
  "agents": {
    "coach": [
      {
        "id": "minecraft",
        "label": "Minecraft tasks",
        "description": "Submit an explicitly requested task to the configured Minecraft presence.",
        "command": ["/usr/bin/python3", "/opt/companion/bridge/minecraft-instruction.py", "--home", "/var/lib/acp-minecraft"]
      },
      {
        "id": "slack",
        "label": "Slack updates",
        "description": "Post an explicitly requested update to the configured Slack destination.",
        "command": ["/opt/companion/bin/node", "/opt/companion/bridge/slack-action.mjs", "--config", "/etc/companion/slack-action.json"]
      }
    ]
  }
}
```

`minecraft-instruction.py --home /absolute/connector-home` runs on the Minecraft
connector's host. It reads that installation's local control configuration and
posts only to its fixed loopback `/task` route. A successful receipt means a new
instruction was **queued**, not that a task was accepted, executed, or completed.
Minecraft records its existing authenticated Operator source; the original voice
caller remains attributed in Companion's private ledger. The helper does not
itself replay game operations, renew tasks, or extend budgets.

`slack-action.mjs --config /absolute/config.json` uses the official Slack SDK to
post the instruction text to one fixed channel. Its configuration contains:

```json
{
  "slack_home": "/var/lib/acp-slack",
  "channel_id": "C0123456789",
  "expected_bot_user_id": "U0123456789"
}
```

Use the installation's actual channel and bot IDs. Credentials remain in its
`secrets/slack.json`; the helper checks bot identity before posting. It preserves
the message text and does not append private transcript provenance. Its
`completed` receipt has `scope: "message_delivery"`: it confirms that Slack returned
the expected channel and message timestamp, not completion of a task described in
the message. Both helpers return uncertain delivery as `unknown` without retries.

When enabled, the per-call bridge uses the official MCP SDK to expose
`companion_connections`, `companion_observations`, `companion_request_action`,
`companion_action_status`, `companion_connection_status`, `companion_show`, `companion_shared`,
`companion_view`, `companion_capture`, `companion_surface_status`,
`companion_hangup`, and `companion_feedback`. Conversations supplies the descriptor
through ordinary ACP `mcpServers`; the agent must advertise HTTP MCP support.
These tools apply only to the private call and selected agent. They do not merge
sessions, imply write authority from context access, or require a shared
Constellation provisioning or identity service.

The backend must be able to reach Conversations' public HTTPS address from its own
network namespace. A working browser connection does not prove that route. For a
containerized agent, verify the fixed destination from inside the container and
use an installation-owned route that preserves its existing isolation and routes.
Do not copy provider credentials into the container or expose the local MCP port;
the authenticated per-call HTTPS endpoint is the intended connection.

With optional actions enabled, Conversations can answer a native ACP permission
request with `allow_once` for its own call tools. The active private call, caller,
agent, and grant must match the dynamically injected `companion` server; a static
server with that reserved name is rejected. The native `rawInput` must identify
that server and a known tool with the exact argument schema, and its title must
agree. A title alone never grants permission. Read/shared/status tools and valid
HTTP(S) show requests qualify; an action also needs the exact authorized active
delegation and a configured connection. The tool API repeats its authority and
receipt checks. The selected agent still interprets the caller's intent; the host
does not independently prove semantic action authorization from the utterance.
Unknown request shapes, other MCP servers, commands, and file operations retain
human approval. This does not create broad or persistent permission grants.

An action requires an active, authenticated caller delegation and a configured
connection. Use the exact `delegation_id` supplied with that caller instruction;
later steering must not silently substitute a different request. The host records a durable receipt before invoking its fixed helper.
The helper receives one JSON document on stdin with `version: 1`, `request_id`,
`instruction`, and private `source` provenance: caller, call, conversation,
delegation, message, original text, timestamp, and revision. It returns one JSON
receipt on stdout with a `state` such as `queued`, `accepted`, `completed`, `failed`,
or `unknown`. Platform setup and credentials belong to the helper's installation.
Preserve source provenance privately rather than publishing the call transcript
into another audience.

A queued task is not evidence of acceptance or completion. An action status lookup
reads the recorded receipt; it is not independent verification of downstream work.
Reusing the same request ID returns the existing receipt rather than resubmitting;
a changed payload is rejected. A timeout, lost response, or restart during
submission leaves an unknown outcome that must not be retried automatically.
Hanging up closes new call-tool actions. The original call grant can still read
saved operation receipts while already accepted backend work settles; it cannot
create a new effect, capture, or live observation. Once that call's tool transport
is disposed, a recognized late Companion tool permission is cancelled rather than
opening an approval for the closed browser. Saved receipts remain stored; this
does not replay work or turn an uncertain delivery into success. Already submitted connector
work retains its original authority and must be inspected through its owning
connector's controls when no retained receipt establishes the outcome.

## API reference

Browser routes use the host session and authenticated ownership checks. Mutations
require the session's `X-CSRF-Token`. All paths below start with `/api/v1`.

| Method and path | Purpose |
| --- | --- |
| `GET /companion` | Contacts, effective voice preference, availability, and an existing open call. |
| `GET /companion/agents/{id}/voice-settings` | Voice choices, configured `voice_provider`/`voice_model`, nullable last-reported `backend`, and `applies_to: "next_call"`. |
| `PUT /companion/agents/{id}/voice-settings` | Save `{ "voice": "marin" }` for this person and agent. |
| `GET /companion/agents/{id}/capabilities` | Fresh initialization probe: `image_input`, `capabilities_available`, `checked_at`, `source`. No model prompt. |
| `POST /companion/calls` | Create with `request_id` UUID, `agent_id`, and SDP `sdp`; retain the same request for uncertain-create reconciliation. |
| `GET /companion/calls/{id}?after={cursor}` | Call state, durable events, and `next_cursor`. |
| `POST /companion/calls/{id}/heartbeat` | Keep the existing call's browser lease alive. |
| `POST /companion/calls/{id}/close` | Begin closure; poll for terminal `closed` or `interrupted`. |
| `POST /conversations/{room}/attachments` | Save `{name, mime_type, data}` with base64 bytes. Returns the attachment ID. |
| `POST /companion/calls/{id}/uploads` | Record `{upload_id, state, source, name, attachment_ids}`. |
| `GET /companion/calls/{id}/results/{message}` | Retrieve a settled public result's full `{message_id, text}`. |

Upload `state` is `captured`, `uploading`, `available`, or `failed`; `source` is
`camera`, `drop`, `paste`, or `file`. Start with capture or upload, keep the UUID,
name, and source stable, and supply one to four owned attachment IDs only for
`available`. Exact duplicate state notices are idempotent. The response separates
saved state from `voice_awareness: "queued" | "not_queued"`.

Optional MCP uses `POST /companion/calls/{id}/mcp`. Its host callbacks under
`/companion/calls/{id}/tools/` include `GET connections`, `GET observations`,
`POST actions`, `GET actions/{request}`, `GET`/`POST shared`, `POST view`,
`POST capture`, `POST hangup`, `POST feedback`, and `GET surface/{operation}`. These use a call-scoped bearer grant,
not browser session authorization, and reject browser Origin requests. The MCP
SDK owns protocol framing; these are Companion application routes, not ACP
extensions. `companion_request_action` takes `connection_id`, `instruction`, the
original `delegation_id` (a provider ID of 1–160 characters), and a UUID `request_id`.
`companion_show` takes a `title` and either an HTTP(S) `url` or bounded plain `text`.
Optional `kind` is `auto`, `link`, `image`, or `video` for a URL, and `note` for text.
Saving a card is neither platform publication nor evidence that it was displayed.

## Validation and remaining boundaries

`npm test --prefix bridge` checks provider controls, official MCP transport,
fixed call authority, safe error results, and closure. Rust tests cover ownership,
capability probes, saved voice preferences, upload state and deduplication,
complete result fragments, and optional action receipts.
`node tests/companion-browser.mjs` runs real Chromium with fake API and media: capture,
paste/drop, uncertain receipts, safe rendering, tab reuse, bounded desktop/mobile
layout, and call/privacy cleanup. These fixtures do not verify a paid provider
call, physical phone, real model image acceptance, or platform action completion;
record those checks separately.

No group media, continuous camera streaming, or voice inside Slack/Minecraft is
enabled here. A future local speech provider can reuse these call and delivery
boundaries without changing ACP runtime ownership. Background awareness remains
separate from an instruction to speak or authority to act.

Official provider references: [Live](https://developers.openai.com/api/docs/guides/live),
[delegation](https://developers.openai.com/api/docs/guides/live-delegation),
[WebRTC](https://developers.openai.com/api/docs/guides/voice-webrtc), and
[server controls](https://developers.openai.com/api/docs/guides/voice-server-controls).

## Hands-free controls and connection awareness

An enabled call MCP server now also exposes `companion_view`,
`companion_capture`, and `companion_surface_status`. A view operation can scroll
one pane, change tabs, or focus an actual shared-card ID. It cannot choose DOM
selectors, run JavaScript, navigate arbitrary pages, or open another call.

The human must enable the camera in the browser and select **Allow photos when
I ask by voice during this call**. Only then can a source-bound delegation request
one photo. It cannot enable a camera or microphone. Turning the camera off,
leaving the call, or losing the browser's live presence closes that permission.
A capture receipt progresses through queued, accepted, executed (one frame), and
uploaded (saved bytes). The browser executes only after its first confirmed
atomic acceptance; uncertain acknowledgments never recapture or resend a photo.
An already queued browser operation can survive successful completion of its
originating reasoning turn until its own expiry. That completion does not grant
a new operation; revoked/unknown sources, expired commands and duplicate
acceptances cannot create another effect.
The status tool can return the actual owned JPEG/PNG as MCP image content for a
reasoning engine only when this call's actual ACP initialization advertises image input. Unsupported or unconfirmed support retains the saved artifact and omits image bytes. Uploaded does not mean inspected. Camera images
from this path are capped at 512 KiB. A local-only engine remains local-only; the
connector does not add a cloud vision fallback.

### Show and help

During a call, choose **Show something** to open the camera preview, then share
one photo or enable one-photo-by-voice consent and ask the agent to look. Merely
opening the camera does not share an image or begin a model request. Explain the
goal naturally; the agent should offer the next useful step and use the existing
whiteboard only when an image, short instruction, or comparison helps. A saved
photo is a moment in time, not continuous vision. Changed scenes require a fresh
deliberate photo when the difference matters.

The optional **Keep screen on** setting uses the browser's screen wake lock only
while the active call is visible. Revocation, hiding the page, ending the call,
or turning the setting off releases it. Unsupported or denied wake lock does not
prevent a call. This is a foreground web experience; it does not promise audio
continuity after locking the phone or switching applications.

An agent-generated screenshot must be explicitly published as an ACP assistant
image to appear as a result. Raw tool images are not automatically shared. Once
published for this call, the agent may inspect that exact saved artifact again
through `companion_surface_status`, without asking for a replacement photo.
The call, selected agent, terminal result and original caller input must match.
Saved JPEG/PNG image delivery is bounded to 2 MiB; phone capture stays at 512 KiB.
Larger saved artifacts retain their identity with an explicit delivery limit.
Neither saving/displaying an image nor returning its bytes proves inspection or
completion of a real-world task.

Browser routes use the owner's authenticated session and CSRF protection:

| Route suffix under `/api/v1/companion/calls/{id}` | Purpose |
| --- | --- |
| `POST /surface/presence` | Exact browser UUID, explicit camera/voice-photo consent, nullable measured audio-activity hints. |
| `GET /surface/commands?browser_id=...` | Pending commands for that browser only. |
| `POST /surface/commands/{operation}/ack` | Atomic acceptance or actual executed/uploaded/failed receipt. |

The immutable operation UUID, exact delegation, call, browser and 30-second TTL
bind each command. Replay after timeout or uncertain execution is refused. A new
UUID cannot create a second photo under the same delegation. Media energy is an
activity hint; it is not speaker identity, transcription, or a finished turn.

### Read-only observers

Connection actions retain their original submission receipts. Optional observers
add subsequent observations alongside them in `companion_action_status`. A
Minecraft task is joined to the exact operator input/source ID, including bounded
retained task history. An agent's `review` status is **reported complete**, with
`verified: false`; it does not become independently verified completion. A new,
unrelated task cannot establish the earlier task's outcome. Slack observations
can prove **message delivery** within the bounded channel history, never acceptance
or completion of subsequent work from message prose. A history gap remains a gap.

Install fixed observer commands and add `companion-awareness.json` to this
Conversations installation. No observer runs unless this separate policy is
enabled. Each source requires exact authorized call owners and exact audience
objects. The following is an example, not a fleet-default policy:

```json
{
  "enabled": true,
  "decision_command": ["/opt/companion/bin/node", "/opt/companion/bridge/awareness-decision.mjs"],
  "guidance": "Prefer substantive requested outcomes or blockers over routine chatter.",
  "agents": {
    "default": [{
      "id": "minecraft-workshop",
      "label": "Workshop",
      "owners": ["local-user"],
      "command": ["/usr/bin/python3", "/opt/companion/bridge/minecraft-observe.py", "--config", "/etc/companion/game-observer.json"],
      "audiences": [{"kind":"public","experience":"minecraft","connection_id":"minecraft-workshop"}]
    }]
  }
}
```

An observer's connection ID must match the corresponding fixed action connection.
Minecraft observer configuration is `{minecraft_home, connection_id,
expected_username, node_executable?}`; Slack configuration is `{slack_home, connection_id,
channel_id, expected_bot_user_id}`. Paths are absolute and installation-owned.
Commands cannot be supplied by speech, a model, or event text. Platform tokens
stay in their existing connector homes. The helpers use only fixed read routes;
the Slack helper uses the official Slack SDK. Each poll has a bounded deadline,
no retries, and at most 32 events and four known action receipts. Source text is
not silently clipped; oversized facts are omitted with coverage information.

When Minecraft supplies its optional `task.limit_state`, the observer can include
that exact current limit summary for the same task and observation time. It
keeps finite, explicitly unbounded, and unknown values distinct, preserves task
audience restrictions, and never attaches current limits to an older action's
outcome. The summary describes current limits, not why a historical task stopped
or whether other activity is allowed. An opaque task cursor suppresses ambient
updates caused only by a new read time or ordinary countdown; actual bound,
usage, expiry, authority, and audience changes remain meaningful. Explicit
status reads retain the exact current values and their immutable revision.

Observations are collected only during an active, authorized private call.
Unsolicited events must match the installed audience policy and be newer than
this call. Older authorized messages and task snapshots remain available for
explicit questions through the read-only `companion_observations` tool; they do
not trigger Decisions, cards, or voice appends. The original source time remains
separate from the time the connector last observed it. A recently observed old
task report is not a new progress report or proof that work is still running.
Private action status is visible only for that call's own exact
request. Disabling or changing the policy withdraws pending presentation work;
revoked credentials, changed room membership, or a changed agent endpoint close
the path. Ending the call stops observations; it does not cancel accepted work.
Durable prior receipts remain available in the installation's call history.

### Decision models and presentation

The official OpenAI SDK's Decisions endpoint with `gpt-6-luna` answers two
independent predicates in one request: whether the event is relevant to the
caller's current topic and whether a proactive spoken mention would be useful.
Both use the intact event, bounded recent transcript fragments, and owner guidance.
Independently bounded human fragments and retained request
IDs/text preserve an explicit selection when a long agent answer fills the recent
transcript; omission counts remain explicit. Decisions receive no credentials and grant no action
permission. The host records intent before each event/context decision, deduplicates
immutable revisions, disables retries, and keeps uncertain outcomes uncertain.
Raw provider probabilities remain in the receipt. The declared pilot policy
requires relevance probability 0.85 or above and proactive probability 0.90 or
above before sending an unsolicited Live update. Relevant `quiet` or `later`
advice retains the evidence locally for cards and authenticated reads; it does
not append to Live as thinking. Thinking can influence speech and is not a silent
transport. Relevance at or below 0.20 is ignored; intermediate estimates also
retain evidence without adding voice context. The host validates the assessment
and its derived disposition independently. Older configured choice helpers must
also select eligible `notify` advice with their existing conservative confidence
checks. Quiet/later is not a promise of automatic later delivery: a new source
revision receives its own decision, and explicit reads can retrieve saved facts.
Provider failure, refusal, and invalid output also retain evidence without adding
it to the speaking model's context. At most two facts are evaluated
per worker cycle. Durable source history has
no lifetime event quota that silently stops a long call receiving new facts;
per-query and per-transport bounds remain. Known action receipts rotate through
bounded polls so newer requests cannot permanently hide older unresolved ones.

An outcome for an already requested action is a result, not optional ambient
news. A validated saved observation bound to its original caller, request and
connection enters context without a relevance-model decision. Status and
qualifications are supplied as context, which can still influence Live speech.
An exact private source reply can be offered as speech,
with its recorded-only provenance supplied first. Only a whole, unambiguous reply
whose complete ordered parts fit the existing append and batch bounds takes this
route; unavailable bodies retain an explicit notice and full history. It still waits for the conversational floor and passes
the same private-audience, source, revision and call checks. Arbitrary observer text cannot claim this route by
labeling itself an action result. A result receipt does not establish fulfillment
or require a spoken interruption. Waiting behind a long human explanation does
not erase a bound result; its original timestamp remains explicit.

When every external action linked to a backend turn has only a known submitting,
queued or accepted receipt, that turn's native prose stays in its original card,
history and authenticated result read. It is not automatically appended to Live:
thinking can be spoken, and a transport receipt establishes neither execution nor
non-execution. This checks all inputs incorporated into that exact target turn;
separate target answers and actual artifact publication remain available. Mixed
same-target independent prose is preserved for inspection, not automatically
split or spoken. Any unknown or terminal helper receipt on that target keeps the
existing answer path so uncertainty or failure is not hidden. Independently
correlated source replies, blockers and interrupted outcomes retain their own
presentation path. Existing queued/sent/unknown result batches are unchanged.
Recorded source replies are not independent proof of physical completion or
hearing. Previously accepted whole replies, including older thinking-only
presentations, are not offered again merely to change their speech channel;
existing unknown append batches are not resent. A genuinely new source revision
may still contain a reply whose earlier context acceptance was unproven.

Full relevant facts are retained as shared cards. Eligible proactive updates are
supplied with their actor, connection, scope and original time. Long facts use numbered source-data parts,
not clipped summaries or opaque retrieval pointers. For a shared connection ID,
use the same user-facing label in action and awareness configuration so the caller's
selected name also identifies background observations. Labels are presentation
metadata; the stable connection ID and caller/source binding establish identity.
Each provider append stays
below its transport bound; all matching receipts are needed to mark the whole
observation accepted. A missing part means the source must not be summarized.
Both observation context and spoken notices wait for the shared conversational floor:
fresh authenticated browser activity, available playback, and a gap after the
latest human transcript. Ambient relevance is evaluated against that current
context and checked again before dispatch. A newer human context withdraws an
unsent proactive batch; a known completed proactive decision can be reconsidered
for the new context, but retained quiet advice is not automatically rescheduled.
An uncertain decision or append is never replayed. Existing unsent quiet batches
are withdrawn before transport with their exact commands retained; dispatched,
accepted and unknown history is not rewritten. These checks are
presentation heuristics, not proof that speech played or a human turn ended. An unavailable
observer does not block the others. Model scores are pilot thresholds, not
calibrated accuracy or a delivery guarantee.

`companion_observations` reads only this call's saved, currently authorized facts.
Each delegated backend request includes a fresh, compact connection catalog when
its Companion tools are installed. The inline catalog and `companion_connections`
use the same projection: this agent's configured action `connections`, their exact
request/status tool names and required fields, plus the caller's authorized
`observation_sources`. Read routes retain exact `connection_id`, label and literal
arguments for saved observations, current snapshots and source reports. A read
operation is labeled read-only; that is not a restriction on the whole connection.
Action and read enrollment remain independent, including action-only installs. These IDs belong to Companion, independently of
Instance context-provider IDs. The catalog runs no helper or model and proves
neither availability nor action authority. Tool execution rechecks access;
omitting the status view still selects reports for existing clients.

For a question about one connection, pass its exact optional `connection_id` from
`companion_connections.observation_sources`. Selection precedes projection and
applies to reports and coverage; unknown or unauthorized IDs return no facts and
never fall back to other connections. Omission retains the all-source read. It
does not contact a service or model. It reports
the latest retained revision per source event, original attribution, observation
freshness, source age, coverage, and omissions. Bounded selection rotates across
connections, preferring task/action reports and then recent original event times;
a busy chat baseline cannot consume every slot. These are saved reports, not proof
that every described task is still current. Missing observations do not prove
inactivity. Source, owner, audience, or call identity changes close access;
stored coverage and cursors cannot cross a changed policy scope.

Interpretation guidance preserves the source's actor and tense: a planned
meeting or transfer remains a plan, and reported completion remains a report.
Even zero omitted saved facts does not establish complete source history or
current inactivity. Spoken summaries should bound absence claims to the returned
snapshot. The voice speaks as the selected agent while keeping other reporters
distinct. These are model instructions, not semantic verification or a filter
that rewrites the recorded result or speech.

Thinking appends are quiet-context intent, not a promise that their contents can
never be spoken. Both thinking and commentary appends cancel pending provider
automatic delegation acknowledgments, so this implementation guards newly created
delegations as described above. Matching provider acknowledgments record
**context accepted**, not audio playback or completed work. Uncertain appends are
never replayed automatically. These are Companion/MCP application policies, not
new ACP fields or a replacement agent runtime.

References: [OpenAI Decisions](https://developers.openai.com/api/docs/guides/decisions),
[Live conversations](https://developers.openai.com/api/docs/guides/live-conversations),
[Live delegation](https://developers.openai.com/api/docs/guides/live-delegation).

## Conversation reliability and evaluation

The speaking prompt follows the GPT-Live role, backchannel, interruption and
client-delegation structure. Detailed procedures stay with the existing ACP
agent. A short, role-labelled recent transcript window supplies conversational
references; it is explicitly context, not a replay of earlier instructions.
The current delegated source remains separate. Runtime identity comes from the
installation profile, distinct from the cloud voice interface.

Multiple inputs can be incorporated into one backend turn. The host settles each
exact source only when its linked target has completed successfully and publishes
that answer once. Source incorporation alone is not completion. Repeated elapsed
time does not generate repeated waiting speech. After hang-up, accepted work can
finish and retain its result, but cannot dispatch another voice request or append
more voice context.

An explicit provider delegation with observed human words can reach an already
active, negotiated steering target before the normal two-second transcript wait.
The target is bound to that live session and prompt; this does not imply the
caller finished speaking. If the target ends or definitively requires a new
prompt, ordinary fallback retains the original source and original two-second
deadline. Empty input, cold starts, and unsupported endpoints keep the normal
wait. A received-only steering acknowledgment remains unknown at terminal unless
the endpoint supplies incorporation evidence; it is never automatically resent.

Results are durably queued separately from the voice transport. Fresh authenticated
browser observations decide when a result may enter the speaking context: both
parties must be quiet and playback available. Unknown or stale observations defer
presentation; a transcript pause is not a semantic turn-completion signal. A newer
unsent revision replaces only the same fact, never a distinct unanswered question.
The final dispatch rechecks private-room membership, endpoint identity and source
policy. A process interruption during dispatch leaves delivery unknown rather
than replaying it. Queue pressure keeps the canonical result available without
disconnecting the call. Provider append acknowledgment does not prove playback.
The call-local pending receipt described below is the only quiet batch that may
cross an occupied floor; result and observation batches keep these checks.

The browser preserves short listening cues but suppresses sustained overlapping
agent speech when the caller takes the floor. Muted audio is consumed and discarded,
not paused for later playback. Fixed application instructions ask GPT-Live to yield;
the browser guard does not rely on the model obeying them. Playback resumes only
after fresh observed quiet, never from buffering, a hidden page, or an instruction
acknowledgment. These acoustic boundaries remain heuristics, not sentence or intent
recognition. Stopping speech does not cancel accepted work.

Current task observations distinguish snapshot time from the original progress
report time. Prior-call context carries human goals and exact result references;
old assistant prose is not automatically reused as current status. Newly submitted
connection actions freeze their observer and original source binding. After hang-up,
read-only polling can collect the exact outcome for up to an hour; a later private
call can continue that lookup after revalidating owner, agent, endpoint, observer
policy and original audience. No new action, model call or retry is performed by
this observer. Older actions without the binding are not retroactively adopted.

Minecraft outcomes may include optional `work_resumptions` from the original
request: task ID, recorded status, `scheduled`, and `execution_started`. Companion
retains those exact bounded facts through the existing caller/source binding.
Scheduling is not execution, a saved start is not completion, and neither is a
fresh activity check. Missing or omitted evidence stays unknown. Empty additive
evidence does not reannounce an unchanged reply; real changes use the existing
semantic revision and polling deduplication. Whole-record omissions are explicit,
and the original source retains the full record. Reading this evidence never
resumes, renews, or replays work.

Native permissions outside the narrowly validated Companion tool policy appear
as single-request choices on the call screen. They are still actor- and
conversation-bound; the voice model does not approve arbitrary native tools.
The call supplies one permission notice instead of describing a hidden approval
wait as service failure.

`companion_hangup` stops this caller's media and requests graceful closure; it
never cancels accepted agent work. The browser retains a silent transport until
terminal confirmation or bounded cleanup. `companion_feedback` saves a concise,
source-bound note privately in the installation and returns a durable receipt.
It does not send a developer message, create an external issue, or copy the whole
transcript. `companion_surface_status` can recover that exact saved receipt after
a lost response. The camera shutter cue is emitted at actual frame capture;
blocked browser sound is shown explicitly, and capture is separate from analysis.

Provider command rejection, speech interruption and transport loss are distinct.
The official SDK emits a server error through both event and error callbacks;
that does not by itself close a session. Rejected appends are not retried. Actual
transport failure or provider terminal state follows the existing bounded close
path. Context acknowledgment, backend success, and audio playback remain separate
observations.

Use repeatable scenarios at three levels:

| Scenario | Observable pass criteria |
| --- | --- |
| Several questions merge into one backend answer | All exact linked successful sources settle; one immutable answer; no duplicate speech or call interruption. |
| Slow or permission-gated tool followed by a new topic | Visible actual permission, no recurring stale waiting speech, no inferred success. |
| Background activity while another task runs | Actor, connection, original time and observation freshness retained; useful context can arrive at a quiet opportunity. |
| Photo, feedback, navigation and hang-up | Real operation receipt, one execution, correct private scope, released devices and separately confirmed provider closure. |
| Interrupted connection or lost acknowledgment | Unknown outcome retained; no automatic effect replay or invented completion. |
| Spoken correction or short reference | Latest source intent preserved; prior context does not authorize another action. |

Run Rust lifecycle/authority tests, official MCP/Live SDK boundary tests, and the
Chromium control-flow scenarios before a bounded real voice check. For paid or
human calls record source audio timing, delegation/permission/tool/result times,
context acknowledgments, media evidence, and final application state separately.
Report sample counts and evidence limits; one successful call is not a latency or
naturalness benchmark. Evaluate repeated multi-turn calls, corrections and
interruption handling as prompts or models evolve.

References: [GPT-Live prompting](https://developers.openai.com/api/docs/guides/live-prompting),
[delegation](https://developers.openai.com/api/docs/guides/live-delegation),
[session lifecycle](https://developers.openai.com/api/docs/guides/live-conversations),
and the [voice evaluation Cookbook](https://developers.openai.com/cookbook/examples/audio/voice_agent_evaluation).

### Phone interruption and continuity

A valid provider `session.closed` confirms finalization; its reason still matters.
`connection_lost` is an interrupted call even when final usage is confirmed. The
call API returns `closure.confirmed`, `closure.provider_reason`, and a human-readable
`closure.reason`. Neither provider closure nor a browser reconnect settles an
agent action. Accepted work keeps its original receipt and is never replayed.

After a suspended browser timer, the client grants one bounded reconciliation
window to check the same call before declaring a control timeout. Returning from
a file picker, regaining focus or becoming visible refreshes that existing call;
it does not create a replacement session, resend SDP, or restart work. An ongoing
outage still releases devices. Mobile operating systems may suspend WebRTC while
backgrounded: foreground Chromium tests are not evidence of background phone
support.

Contacts and Conversations remain reachable while an earlier call's closure is
unconfirmed. Dismissing an error changes presentation only. The recovery panel
stays available from Contacts, and new calls remain disabled until the earlier
call is reconciled. A read-only check clears a known call only after a matching
terminal response; an empty catalog is not proof of settlement. Unknown creation
requests keep their original identity and require explicit recovery. Late checks
or close failures cannot reintroduce an old call after recovery or sign-out.

Heartbeat and close requests may include strictly projected `diagnostics`: fixed
lifecycle/transport state enums and bounded timing counters only. The installation
retains the latest snapshot and at most 120 transitions per call. No SDP, addresses,
device names, arbitrary errors, or speech is collected through this path. These
are browser-reported observations, not authorization or playback receipts. The
first application close cause is retained separately from the provider's reason.

A new private call can receive a bounded saved handoff from the latest terminal
call within 24 hours to the same owner, agent, and endpoint. Both rooms must still
be private; changed membership, endpoint, or a formerly shared room excludes it.
Historical source fragments retain their timing and partial status. Exact pending
input/result references may be refreshed when later backend results settle; no
request is resubmitted. Generated speech never implies that the caller heard it.

Camera selection uses only browser-granted video devices. Changing cameras leaves
the voice microphone alone; Camera off releases a late camera acquisition too.
An agent's native tool permission cannot substitute for browser photo consent.
A blocked photo request returns the actual prerequisite without queueing a capture.

For a fresh status request, `companion_connection_status` reads only the selected
installation-enrolled observation helper with no action IDs. It rechecks the
private caller, endpoint and audience policy after the bounded read. Source report
times remain separate from query time. Missing facts or a failed read do not mean
inactivity. This avoids a redundant native approval for an already-enrolled read;
it does not allow arbitrary external tools. Expired call-scoped native requests
are rejected directly instead of creating approvals on a closed call screen.
Current task and physical-state snapshots take priority over historical chatter
in the bounded result. Deferred source records remain available through their
unchanged cursor identity; prioritization does not change their original time.

Use `view: "current"` for a question about current position, presence or
assignment. This selects only typed presence/assignment snapshots observed during
the fresh query (with the same five-second clock tolerance as the source envelope). Tasks
require their own `observed_at`; presence uses its own `observed_at`, or
`occurred_at` for the installed cursorless observer. It never dates an old
snapshot from the wrapper query time or refreshes nested `progress.at`. The
original source ID, revision, actor, audience, snapshot kind and times accompany
each whole fact.
`selection.excluded_reports` counts authorized reports outside this view,
including stale or untyped reports, independently of response-bound `omitted` and
the source's own
`coverage.omitted_events`. A generic observer without typed current snapshots
returns `no_typed_current_facts`, not an idle claim or an inference from untyped
helper flags. Exclusion does not prove that history is absent; missing facts do
not prove inactivity. Omitted `view` (or `"reports"`) preserves the existing
current-plus-history response.

The full qualified result returns to the backend that requested the read and stays
in its canonical tool history. Polling does not create a second Live context or
speech stream. The ordinary backend answer and independently source-bound action
outcomes remain separate presentation paths. Historical qualifier records and
already queued or uncertain batches are preserved; new reads do not replay or
rewrite them. Private caller, source and policy checks still apply after each
in-flight read, including failed or empty reads.

Pure input queue/admission bookkeeping stays in its original receipt and card,
without an unsolicited Live update. Meaningful source replies, recorded blockers,
interruptions, unknown outcomes, task authority and resumption facts retain their
existing qualifications. Turn scope and ordinary handling-ended bookkeeping are
not standalone progress or completion announcements.

Validated private replies are mapped from their complete source record before
voice formatting. Whole newline-inclusive units may be packed into ordered
480-byte appends; concatenating their bodies reproduces the original reply
exactly. Source qualifications precede the complete body group. Every part and
qualification must be accepted before a later cumulative observation can omit
that exact reply. Acceptance does not prove it was spoken or heard. Unchanged
fully accepted snapshots do not repeat their qualifications merely because the
observation revision changed; changed replies, blockers, authority and coverage
remain eligible. Legacy queued, sent and uncertain batches are never rewritten.
If a whole unit or complete reply group cannot fit the existing bounded batch,
only an explicit unavailable notice is offered; the full reply stays in READ.

### Coherent speech and bounded presentation

Provider transcript deltas are fragments, not conversational turns. The host
groups adjacent fragments from the same speaker without changing their text,
while retaining every original event ID, timing, and fragment in the existing
delegation record. Limits apply to the compact presentation instead of repeated
per-word metadata. Recent coherent speech helps interpret a short “yes” or a
correction. Held input is retained and can be carried forward by a later request;
it is never labelled completed or silently dropped. An actually oversized request
remains saved with an explicit capacity explanation.

If an interrupted earlier response blocks delivery, the call receives one clear
explanation for that blocking set. Later requests remain saved. Status reads and
hang-up never release the block or retry an uncertain action. Structured driver
diagnostics retain only bounded error categories/codes, not raw provider errors.
Results refer to the human request rather than the generated backend prompt;
late results remain available without taking over the current topic.

`companion_surface_status` with no arguments reads current browser presence,
camera/consent facts and saved-artifact metadata. With `artifact_id`, it retrieves
an already shared attachment from this private call, subject to the backend's
actual image capability. It does not recapture, re-upload or claim inspection.
An `operation_id` still reads the exact operation receipt; these selectors are
mutually exclusive.

Whiteboard operations reuse `companion_view` and saved identities:
`replace_board` takes up to three `card_ids`; `add_card` and `remove_card` take
`card_id`; `clear_board` takes neither. Cards are saved event sequence IDs or
`upload:<uuid>` for an available upload belonging to this call. Browser presence
reports current/pinned/focused IDs. `board_full` and `item_protected` are explicit
failures. `companion_show` returns `saved`/`requested_current`; consult browser
presence for display, never infer playback or human attention.
To write a short note, `companion_show` accepts `text` (at most 1,200 UTF-8 bytes)
instead of `url`, with a title and optional `kind: "note"`. The caller sees plain
text; HTML and media references in a note are not executed or automatically loaded.
Existing URL/media calls retain their shape.

### Independent voice hang-up

An installation may set `native_call_control: true` on a Companion profile after
qualifying that project's native Decisions access and spoken behavior. It is off
by default; existing projects do not gain a new provider prerequisite silently.
No failed creation is retried with a different configuration.

The official GPT-Live input-transcript Decisions trigger evaluates only whether
the human expresses a complete current intention to end the call, including a
polite indirect request or a clear closing farewell. A bare “thanks” or ending a
work step is insufficient. It runs independently of the reasoning queue. Its sole effect is existing call closure; it cannot cancel
agent work, resolve unknown delivery or affect another call. Quoted, hypothetical,
incomplete, negated and revoked requests, and requests to stop other work, are
excluded. The host requires the exact current session and source event/times,
a matching completed predicate result, probability at least 0.98 and a short
interval without newer input. Old, conflicting and failed decisions do nothing.
A matching provider terminal event still confirms closure separately.

This native decision may incur provider usage and requires the project's
Decisions access. Automated signal/ordering tests do not establish semantic
accuracy for real speech. Qualify with explicit, quoted, corrected and negative
utterances, natural farewells, incomplete phrases, and stop-work requests before
treating it as dependable hands-free control; the on-screen
End call button remains available independently. See OpenAI's
[voice Decisions guidance](https://developers.openai.com/api/docs/guides/decisions-voice).


### Direct connection context and receipt history

The Context tab shows retained source observations, exact context supplied to
voice, and correlated delivery receipts. "Accepted by voice service" establishes
context delivery only; it does not establish that a model used it or that speech
was played. Historical content without a retained receipt remains unknown.
`companion_view` can open this tab with `action: "show_context"`.

At delegation receipt the host sends the documented empty thinking append to
cancel a pending automatic acknowledgment. After durable intake, one quiet,
source-bound fact records the pending answer and unknown external execution as
compact JSON, rather than ready-to-speak receipt prose. Its source ID and stable
request revision bind the exact original request; a short caller-request label
is included whole when it fits the existing 480-byte budget. No polling clock
creates a new revision. This supports an explicit status question without
requesting another acknowledgment or establishing external progress. Unchanged
polls do not add context; a terminal result withdraws only that request's unsent
pending fact. Previously retained prose stays byte-exact under the same event
identity and current source checks; it is not replaced by a second typed fact.
Accepted or unknown deliveries stay in the audit and are never replayed.

This exact singleton `thinking` receipt may reach voice while the caller or agent
is speaking, or just after a human transcript, so a request's saved status need
not wait for silence. The host still requires fresh, known browser/playback
evidence and rechecks the exact private source, request text, event identity and
absence of a terminal answer at the durable dispatch claim. Missing or stale
evidence holds it. A terminal before claim supersedes it; a terminal after claim
follows it through the same serialized writer. Unknown delivery is never retried.

While speech is held, the oldest eligible receipt may pass older held speech
batches; speech batches keep their order and their floor checks. No other thinking
context, mixed batch, commentary or instruction gains this exception. This uses
the existing `thinking.append` behavior, which does not request speech but can
inform later speech and cancels pending automatic delegation acknowledgments.
Provider acceptance proves neither immediate model use nor silence. Earlier
awareness does not guarantee the model will avoid another delegation.

Independent answers remain eligible when newer questions arrive. Chronology
alone does not mean cancellation or replacement; the conversational floor still
defers delivery while the caller speaks. Image-board placement retains its own
current-result policy. Context acceptance does not prove speech or playback.

Relevant attributed observations are supplied directly to voice. Facts from one
connection must not answer a question about another connection, even when the
agent identity is shared. A long observation
is delivered as numbered source-data parts, preserving its full qualifiers; it no
longer requires a second reasoning turn merely to retrieve its text. All matching
part acknowledgments are required before the recorded observation becomes
`context_accepted`. Observation context can inform speech, so it follows the same
current-context and conversational-floor checks as spoken notifications.

Cumulative requested outcomes keep their full observations, cards and READ
history. Voice presentation can omit an unchanged whole reply only when its exact
source, request, audience and reply record match a prior fully accepted context,
and the matching reply command is retained. Stored, deferred, omitted or uncertain
parts do not qualify. Changed replies and corrections remain available; this is
context deduplication, not evidence that anyone heard a reply. The bounded recent
history lookup keeps the full part when proof is incomplete. A late acknowledgement
does not rewrite an already queued newer batch, so that race can still repeat a
reply without retrying the original provider event.

See [Minecraft observations](companion-minecraft-observation.md) for the official
MCP SDK presence client. It reads own and other players' public chat, current
position and nearby players. Local conversation admission, a settled turn, a
reply, task acceptance and fulfillment remain distinct. Unknown effects are never
replayed to fill a missing receipt.

After media closes, the original per-call MCP listener can stay available while
already accepted backend work settles. Only authenticated saved receipt reads
remain available; ending a call does not reopen actions or another call's context.
Native hang-up diagnostics retain bounded decision status and source identities,
not private reasoning or raw provider configuration.

### Reproducible duplex evaluation

`tests/companion-duplex-eval.mjs` provides deterministic self-checks, a real browser
audio-graph check, and opt-in `--run-live CONFIG` evaluation. Live evaluation requires
an explicitly marked disposable loopback installation and synthetic caller identity;
it refuses the production preview address. The actual Companion UI/API/official
GPT-Live bridge is paired with an independent synthetic caller. Both audio channels
share one browser clock, and the caller hears the application's post-playback-gate
output. Hidden expected outcomes are not sent to the caller model. Domain tools
remain disposable fixtures; record their effects and receipts separately.

Retain scenario, binary/configuration hashes, application events, source-to-result
links, PCM audio and final state. Review coherent answers, corrections, honest
blockers, silence and overlap in context; overlap counts alone are not a naturalness
score. Headless audio and simulated callers do not establish physical phone speaker,
echo cancellation, background operation, or human usability.
