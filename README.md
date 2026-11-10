# Agent Conversations

Part of [Amplifier Constellation](https://github.com/microsoft/amplifier-constellation).

A standalone web experience for talking to an **existing ACP agent**. Each conversation has its own working session, while the endpoint retains the agent's identity and its authorized continuity across Slack, native clients, and other experiences.

This repository contains an ACP client, a private conversation store, a browser UI, and an optional context-provider API. It does not install an agent runtime, assign a persona, read another application's database, or manage a fleet.

The service supports explicitly allowed local users, private conversations, and shared rooms with several independently configured ACP agents. Existing conversations stay private on upgrade. See [shared rooms](docs/shared-rooms.md) for setup, participation, attachments, and audience boundaries. Multiple conversations can run concurrently against the configured endpoints. Imported transcripts from other experiences are viewable here, with their original surface identified, but remain read-only.

[Observable huddles](docs/huddles.md) keep routine agent coordination available
for inspection while meaningful results return to the original request. External
participants keep their existing sessions; inspection and follow preferences do
not grant work authority or start another agent.

## Companion preview

[Constellation Companion](docs/companion.md) adds full-duplex GPT-Live calls to
existing ACP agents, with per-agent voices and separate background work. It has
its own `/companion` interface and optional server-side bridge; existing text
conversations and browser dictation keep their current behavior.

## Phone and voice

The responsive browser experience can be added to a phone or desktop home screen
using its Install/Share menu. It uses the same authenticated service and ACP
connections. The install shell never caches transcripts or credentials for
offline use; reconnect to your network/Tailscale to use your agents.

Press **Start voice** once in each page and grant microphone access. Speech
drafts a message; **“Workshop, send message”** submits it through the ordinary
conversation API. **Read replies aloud** queues completed replies. Voice pauses
while reading and while the page is hidden. Browser speech support varies, and
its speech service may process audio remotely. Typing and device keyboard
dictation remain available when browser recognition is absent.

Commands also include new conversation, clear draft, stop agent, show activity,
show connection, show participants, hide participants, read last reply, stop
reading, stop listening, and open conversation followed by an exact title.
They require the “Workshop” prefix. This is a hands-free path for existing
conversation/view controls, not a second agent or an always-listening assistant.

For a single pending action, **read approval**, **show action details**,
**allow once** and **reject once** use the same person-bound permission endpoint
as the buttons. Ambiguous multiple requests require selection on screen;
voice never chooses an action or grants a permanent policy implicitly.
**Reply to [agent name/everyone]** and **name conversation [title]** control
the existing target picker and conversation title.

Voice preferences and help visibility use authenticated view state. The browser
also exposes `window.agentConversationsControl.perform({action, text})`,
`.voice.transcript(text)`, `.view()` and `.setView(changes)` for authorized UI
automation. Remote preferences cannot grant microphone consent. All ordinary
conversation and view APIs remain usable without the packaged UI.

Agents can also ask structured questions through standard ACP form elicitation.
The question names its requesting agent and appears only to the person who
started the turn. Review or edit the fields, then send, decline or cancel.
Drafts survive page reload and can be prepared through view control; preparing a
draft does not submit it. Forms collect ordinary task information, not credentials
or permission to run tools. URL elicitation is not advertised. See the
[form API](docs/api.md#structured-questions) for automation and constraints.

## Build and install

On Ubuntu, install the Rust toolchain and build prerequisites (`build-essential`, `pkg-config`, `libpam0g-dev`, and `libclang-dev`), then:

```sh
cargo build --release
install -Dm755 target/release/agent-conversations "$HOME/.local/bin/agent-conversations"
sudo install -m644 ops/pam-agent-conversations /etc/pam.d/agent-conversations
```

PAM supports each explicitly allowed local account’s ordinary password. The host account must permit PAM authentication. This release is tested on Ubuntu; Windows and native mobile authentication are not implemented.

Initialize a separate installation folder for this experience:

```sh
agent-conversations --home "$HOME/.local/share/agent-conversations/helper" init \
  --agent-home "$HOME/.local/share/agent-instance/first-agent" \
  --agent-label Helper \
  --owner "$USER" \
  --bind 0.0.0.0 \
  --port 38621 \
  --public-url https://agent.example.test:18445 \
  --tls-cert /private/path/conversations-cert.pem \
  --tls-key /private/path/conversations-key.pem \
  --tls-ca /private/path/root-ca.crt
```

The certificate paths are an explicit operator choice. Supply this service's
own certificate and private key, or omit the TLS arguments to generate them.
Use your own reachable hostname for `--public-url`; example domains are placeholders.
The service never reads the agent's private transcripts or state database.

For another ACP implementation, use `--agent-command`, repeated `--agent-arg`, and `--agent-cwd` in place of `--agent-home`. The command must speak standard newline-delimited ACP JSON-RPC over stdin/stdout. No shell evaluates its arguments. Remote attach commands, including an explicit SSH command, can be configured the same way.

Without certificate arguments, initialization generates a local CA and certificate for localhost and the machine's current IP addresses. Trust `tls/root-ca.crt` on clients, or put a trusted HTTPS proxy in front. No password is accepted over plain HTTP. A Tailscale IP by itself does not provide browser-trusted HTTPS; use a configured Tailscale HTTPS DNS endpoint.

Start the service:

```sh
agent-conversations --home "$HOME/.local/share/agent-conversations/helper" serve
```

The selected port is written to `runtime.json`; port `0` selects a free port. `status` prints the last listener information and `probe` negotiates ACP without sending a model prompt. The systemd user-service example in `ops/` can be adapted to your installation path. Every installation has an independent folder and lock; two servers cannot accidentally own the same folder.

## Connect the agent's continuity service

Initialization generates owner-control, context-read and legacy Toad-import credentials. Additional experiences get their own named import credentials:

| File under the installation folder | Purpose |
| --- | --- |
| `secrets/control-token` | Owner administration, conversations, and view control. |
| `secrets/context-token` | Legacy context provider restricted to the original default agent and its imported history. Do not reuse this credential for another agent. |
| `secrets/import-token` | Original Toad import credential, preserved on upgrade. No context reads, chat, configuration, or UI access. |
| `secrets/import-sources/<source>-token` | Independently scoped import credential for another experience, such as Zed. |

Before the first chat with a newly registered Agent Instance endpoint, issue its own source credential. Replace `native` with its ID in this experience's agent registry:

```sh
agent-conversations --home "$HOME/.local/share/agent-conversations/helper" context-source issue native --token-out ./native-context-token
```

The command saves a private file and prints connection metadata, never the token. Issuing again rotates the credential; `context-source show native` inspects it and `context-source revoke native` disables it. Credentials expire after 90 days by default. Store the saved token as a named secret in that agent, then register the provider in its optional context registry. `status` prints the stable ID, `web:<installation UUID>`:

```json
{
  "id": "web:<installation UUID>",
  "label": "My web conversations",
  "url": "https://agent.example.test:18445/agents/native",
  "secret_ref": "web-conversations"
}
```

Agent Instance exposes `PUT /api/v1/secrets/{name}` with `{ "value": "…" }`, and `PUT /api/v1/context/providers` with `{ "providers": [ … ] }`. Preserve existing provider entries when updating that registry. Supply credentials through protected files or API bodies, never command arguments or chat messages. The agent must trust the provider's HTTPS certificate.

On each ACP attach, this client sends an expiring conversation grant in `_meta["io.github.bkrabach.conversation"] = {provider_id, grant}`. The grant is not placed in prompts, transcripts, or activity logs. Ordinary ACP peers may ignore this optional metadata; configured `agent.mcp_servers` descriptors are passed through unchanged on new/load. Agent Instance adds its own authorized context tools independently.

Owner-private conversations retain an owner grant for authorized continuity. Shared conversations and other people's private conversations receive room-only grants. Explicit audience changes revoke grants and rebuild working sessions. The provider enforces this source boundary; the agent service still controls its own retrieval, memory, and outgoing disclosures.

Each new agent source further limits all reads to rooms that agent currently participates in. Removing the agent from a room removes source access immediately, including access using earlier grants. It cannot retrieve another agent's private rooms or imported transcripts that have no attached agent. The original `/context/v1/query` route keeps its credential but now enforces the same participation boundary for the default agent, including its imported Toad/Zed history. Existing and future import bridges currently belong to that default agent. New agents use their own scoped route and credential.

## Other ACP experiences

An optional bridge can copy its own **public ACP observations** into this experience through the import API. It never needs the agent's session files. Every importer has an independently scoped source name, fixed surface, and credential. A Zed token cannot import into Toad, label itself as Slack, read conversations, or overwrite native web conversations. Existing installations migrate their original `secrets/import-token`, source, surface and conversation IDs unchanged. The legacy `config.import_source` / `config.import_surface` fields only seed that migration; use the source API or CLI for subsequent management.

Create a separate source for each authorized experience, using the running service:

```sh
agent-conversations --home "$HOME/.local/share/agent-conversations/helper" \
  import-source create zed --surface zed --token-out /private/zed-import-token
agent-conversations --home "$HOME/.local/share/agent-conversations/helper" import-source list
```

Create the private destination folder first. The CLI writes the new credential with mode `600` and prints only its location. `import-source rotate zed --token-out /private/zed-import-token` immediately replaces the old token; reconnect the bridge to pick it up. `import-source revoke zed` stops future imports without deleting existing history or affecting Toad. Rotation, or creation of a previously revoked source with the same surface, explicitly re-enables it. Revocation survives service restarts. Import configuration and credential management have equivalent authenticated APIs; no agent-management API is required.

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

## APIs and view control

All browser operations use the same APIs available to tools. See [the API contract](docs/api.md) for conversation, source, and view-control shapes.

The CLI can call an authenticated local API without printing a credential:

```sh
agent-conversations --home "$HOME/.local/share/agent-conversations/helper" api GET /api/v1/conversations
agent-conversations --home "$HOME/.local/share/agent-conversations/helper" api POST /api/v1/conversations --body-file new-conversation.json
```

Configuration is editable as a JSON file or through `config get`, `config set --file …`, and the admin configuration API. Changes take effect after restarting this web service. Identity and owner are installation settings. Keep credential values out of command arguments and non-secret environment settings.

## Verification and current limits

```sh
cargo fmt --check
cargo test
cargo build
python3 tests/import_sources_cli.py
python3 tests/browser.py
BROWSER_STEERING=1 python3 tests/browser.py
```

The source-management test exercises a real HTTPS service through the CLI: create, scope, rotate, idempotent import and revoke across restart, including private credential output. The browser test uses an actual HTTPS service and model-free ACP subprocess, a disposable owner session, and Chromium. Set `PLAYWRIGHT_MODULE`, `CHROMIUM_EXECUTABLE`, and optionally `NODE_BIN` for your environment. `AGENT_CONVERSATIONS_BINARY` selects an already-built binary; `BROWSER_ARTIFACT_DIR` keeps temporary fixtures and screenshots outside the checkout. The fixture seeds only its own temporary browser session; it does not claim to verify a real account's PAM password.

This release supports private and shared conversations, independent agent participants, session resume, bounded history reconstruction and search, text/image/PDF attachments with capability-aware fallback, visible progress, cancellation, source retrieval, and programmatic UI control. Agent-managed browser sign-in and structured form/URL questions use the initiating person’s private controls. Interactive tool permission requests appear only to the person who triggered the turn, with explicit one-use decisions and expiry. Optional [local ACP filesystem and terminal callbacks](docs/client-callbacks.md) require owner-configured workspaces and audiences plus approval for each operation; existing connections keep these disabled. [Follow-up messages during work](docs/input-continuity.md) use durable per-recipient intake, negotiated steering, ordinary ACP queuing, and explicit recovery of unknown outcomes. Fleet management remains separate work. Deleting a web transcript revokes its source grants but cannot erase knowledge already incorporated into an agent's own working context.
# Local and remote working folders

An ACP connection's `cwd` is the local folder in which Conversations launches
the configured command. Set optional `session_cwd` when the agent runs on another
machine: it is sent unchanged in ACP session creation and restoration. For
example, a Linux-hosted experience can launch its local attach executable while
requesting `/Users/owner/agent/workspace` on a Mac. No path translation or file
sharing is inferred. Client filesystem and terminal callbacks remain governed
by their separately configured local roots and audience policy.

## Contributing

> [!NOTE]
> This project is not currently accepting external contributions, but we're actively working toward opening this up. We value community input and look forward to collaborating in the future. For now, feel free to fork and experiment!

Most contributions require you to agree to a
Contributor License Agreement (CLA) declaring that you have the right to, and actually do, grant us
the rights to use your contribution. For details, visit [Contributor License Agreements](https://cla.opensource.microsoft.com).

When you submit a pull request, a CLA bot will automatically determine whether you need to provide
a CLA and decorate the PR appropriately (e.g., status check, comment). Simply follow the instructions
provided by the bot. You will only need to do this once across all repos using our CLA.

This project has adopted the [Microsoft Open Source Code of Conduct](https://opensource.microsoft.com/codeofconduct/).
For more information see the [Code of Conduct FAQ](https://opensource.microsoft.com/codeofconduct/faq/) or
contact [opencode@microsoft.com](mailto:opencode@microsoft.com) with any additional questions or comments.

## Trademarks

This project may contain trademarks or logos for projects, products, or services. Authorized use of Microsoft
trademarks or logos is subject to and must follow
[Microsoft's Trademark & Brand Guidelines](https://www.microsoft.com/legal/intellectualproperty/trademarks/usage/general).
Use of Microsoft trademarks or logos in modified versions of this project must not cause confusion or imply Microsoft sponsorship.
Any use of third-party trademarks or logos are subject to those third-party's policies.

### Optional shared group attention

Ambient group turns discover `com.microsoft.amplifier.attention` v1 on the ACP
endpoint. The browser service supplies bounded delivered room messages and the
original trigger timestamp; Instance supplies social policy and a one-use offer.
A hold removes the empty response placeholder without sending a model prompt.
Source changes and cancellation discard the offer. Engines without this optional
host feature continue to receive ordinary bounded ACP participation prompts.
Direct conversations and explicitly selected recipients retain ordinary ACP.

ACP updates remain available as they arrive. Direct chat displays streamed text;
shared rooms buffer draft text until the final public contribution or `[[PASS]]`
is known. This is a presentation choice, not missing streaming in the agent.
