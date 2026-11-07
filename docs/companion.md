# Constellation Companion (preview)

Companion is a foreground, web-first voice experience for an existing ACP agent.
Open `/companion`, choose a saved contact, and start a call. One person calls one
agent at a time. Each call has its own private conversation and backend session;
it does not reuse or replace a Slack, Minecraft, or another person's session.

The first provider is OpenAI GPT-Live (`gpt-live-1`), with **client delegation**.
GPT-Live handles the spoken conversation while the selected agent handles work
through the same official ACP SDK connection used by Conversations. The chosen
agent runtime is preserved. A local-model agent still uses cloud audio and
selected text context for this voice provider; this is not a fully local call.

## Installation

Use a separate Conversations installation for the preview. Existing text rooms
and browser dictation remain available. Build Conversations normally, and install
the voice bridge using Node 22.6 or later:

```sh
cd bridge
npm ci --ignore-scripts
npm test
```

Place `bridge/` and its locked dependencies in an installation-owned directory.
Set `OPENAI_API_KEY` in the **service** environment, preferably from a private
service environment/credential file. It is never sent to the browser or copied
into an ACP agent's environment. The official OpenAI SDK owns HTTP/WebSocket
transport. The Rust Conversations client uses the official ACP SDK through
`amplifier-acp-connection`; this feature adds no ACP wire methods.

Register the existing ACP endpoints through Conversations' saved-agent API.
Remote agents can use fixed SSH stdio attach commands; use credential files on
the agent's own host instead of putting secrets in command arguments. Add
`companion.json` to this installation home, using its actual saved agent IDs:

```json
{
  "enabled": true,
  "node": "/absolute/path/to/node",
  "bridge": "/absolute/path/to/bridge/live.mjs",
  "max_call_seconds": 1800,
  "profiles": {
    "coach": {
      "voice": "marin",
      "description": "Native reasoning · OpenAI cloud voice",
      "context_registered": false
    },
    "rook": {
      "voice": "cedar",
      "description": "Codex reasoning · OpenAI cloud voice",
      "context_registered": false
    },
    "jarvis": {
      "voice": "ash",
      "description": "Local reasoning · OpenAI cloud voice",
      "context_registered": false
    }
  }
}
```

Voice profiles are separate from ACP endpoint configuration: changing a voice
must not reset an agent's working sessions. Voices are fixed for a call; changes
apply to the next call. There is no silent provider or voice fallback.

`context_registered` stays false until this installation's optional transcript
provider has actually been registered and made reachable by that agent. It is
not a request to provision a provider. With false, call transcripts remain local
and bounded relevant speech accompanies explicit delegation over ordinary ACP.
The agent retains access to its already-authorized sources. Automatic awareness
subscriptions across other experiences are not part of this preview.

Serve behind trusted HTTPS, including on phones. Browser microphone and camera
permissions are granted locally. The host's normal PAM login protects access;
opening the page is not microphone consent. This is a foreground call, not a
promise of iOS/Android background operation. Active silent or muted GPT-Live
sessions may still incur provider charges. The preview closes calls after its
configured maximum duration or when browser heartbeats stop.

The signed-in person authorizes the microphone channel; this preview does not
identify speakers by voice. Existing agent permissions and work controls still
apply to delegated requests. Multi-person speaker identity is a later boundary,
not something inferred from a transcript's wording.

## What each signal means

- **Connected:** media and voice control are established, not proof that the
  backend has accepted work.
- **Delegated:** a timed provider request was linked to the call's authenticated
  user and transcript, then submitted through the ordinary ACP delivery path.
- **Result available:** a public backend answer settled. The full answer remains
  on screen; a short excerpt can be injected into voice context.
- **Context acknowledged:** the provider accepted an append. This does not prove
  that it spoke it or that the person heard it.
- **Audio playing:** the browser observed output playback. Transcript generation
  alone is not a playback receipt.

Provider transcript events are fragments, not complete-turn markers. They are
retained with their original event IDs and timeline positions. Delegation events
contain an ID and timeline offset, **not a task description**. The host assembles
bounded call context and holds ambiguous/missing input rather than inventing a
request. Durable delegation IDs prevent duplicate backend submission. Unknown
admission is never automatically replayed.

Hanging up closes the voice session and releases local media. It does **not**
cancel already accepted work. The linked conversation has the existing controls
for reviewing permissions and stopping work. Muting only changes microphone
input. Native voice interruption does not itself retract an already submitted
ACP operation.

## Camera and display

Camera access is optional and starts only after a local action. A preview is not
proof of image understanding. GPT-Live does not accept images/video directly.
An explicitly submitted snapshot follows the existing attachment/ACP capability
path, so an unsupported backend cannot pretend it saw it. There is no automatic
cloud vision fallback for local-model agents. The conversation remains the
review surface for full answers, generated artifacts, and permission requests.

## Boundaries for the next iteration

The bridge is independent of browser layout. A future local STT → agent → TTS
provider should implement the same call, transcript, delegation, interruption,
and playback concepts without changing ACP runtime ownership. The newline JSON
between Rust and its fixed child bridge is a private application control channel,
not an ACP extension or an arbitrary host-command API.

Future rooms will allocate one independent realtime session per agent. Joining a
room will require explicit audience/history rules; current text-room membership
is not an implicit voice invitation. No group media, voice in Slack/Minecraft,
or continuous camera streaming is enabled here.

Meaningful awareness will need permission, provenance, freshness and duplicate
checks before optional decision-model relevance and quiet voice-context
injection. It must distinguish awareness from an instruction to speak and from
authority to act. GPT-Live's Decisions triggers are an available provider feature;
the preview leaves them off while the event policy is designed.

## Validation

`npm test --prefix bridge` exercises transport authority, duplicate commands,
redacted errors, bounded spoken excerpts, and confirmed versus unknown closure.
Rust Companion tests exercise authenticated ownership, durable delegation and
source context. The browser fixture exercises media lifecycle without a paid
provider call. Live API and physical-phone checks should be recorded separately
from fixture success, including selected backend/runtime and actual terminal
settlement.

Official provider references: [Live](https://developers.openai.com/api/docs/guides/live),
[delegation](https://developers.openai.com/api/docs/guides/live-delegation),
[WebRTC](https://developers.openai.com/api/docs/guides/voice-webrtc), and
[server controls](https://developers.openai.com/api/docs/guides/voice-server-controls).
