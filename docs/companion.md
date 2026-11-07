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

The shared pane and transcript scroll independently of the call controls, on
desktop and mobile. Conversation and work-control links reuse a separate work tab
so the call can remain open. Answer links are clickable. Supported saved media
can appear inline; remote HTTP(S) media previews require a click. An ordinary
page or repository link remains a link, and a displayed video does not prove it
was played or watched. Answer text is never executed as HTML or JavaScript.

Choose **Voice** on a contact, or **Voice settings · next call** during a call.
Preferences are saved for the signed-in person and selected agent. A change takes
effect on their next call; it does not change the current voice, ACP identity,
model, or working session.

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
  "max_call_seconds": 1800,
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
Maximum call duration and browser heartbeats bound unattended sessions.

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
`companion_connections`, `companion_request_action`, `companion_action_status`,
`companion_show`, and `companion_shared`. Conversations supplies the descriptor
through ordinary ACP `mcpServers`; the agent must advertise HTTP MCP support.
These tools apply only to the private call and selected agent. They do not merge
sessions, imply write authority from context access, or require a shared
Constellation provisioning or identity service.

An action requires an active, authenticated caller delegation and a configured
connection. The host records a durable receipt before invoking its fixed helper.
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
Hanging up revokes call-tool access, including status access through that expired
call. Already submitted connector work retains its original authority and must be
inspected through the owning connector's controls.

## API reference

Browser routes use the host session and authenticated ownership checks. Mutations
require the session's `X-CSRF-Token`. All paths below start with `/api/v1`.

| Method and path | Purpose |
| --- | --- |
| `GET /companion` | Contacts, effective voice preference, availability, and an existing open call. |
| `GET /companion/agents/{id}/voice-settings` | `voice`, `available_voices`, and `applies_to: "next_call"`. |
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
`/companion/calls/{id}/tools/` are `GET connections`, `POST actions`,
`GET actions/{request}`, and `GET`/`POST shared`. These use a call-scoped bearer grant,
not browser session authorization, and reject browser Origin requests. The MCP
SDK owns protocol framing; these are Companion application routes, not ACP
extensions. `companion_request_action` takes `connection_id`, `instruction`, and a
UUID `request_id`; `companion_show` takes an HTTP(S) `url`, `title`, and optional
`kind` (`auto`, `link`, `image`, or `video`). Showing a link is neither platform
publication nor evidence that the media loaded.

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
