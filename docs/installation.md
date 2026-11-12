# Installation and connection configuration

These commands configure the current Conversations service. Run repository build
commands from the checkout root. The [interface map](README.md) locates the
service contracts; [validation](validation.md) covers reproducible checks.

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

The [transcript import contract](api.md#transcript-imports) defines the payload,
append-only behavior, limits and returned conversation.

## APIs and view control

All browser operations use the same APIs available to tools. See [the API contract](api.md) for conversation, source, and view-control shapes.

The CLI can call an authenticated local API without printing a credential:

```sh
agent-conversations --home "$HOME/.local/share/agent-conversations/helper" api GET /api/v1/conversations
agent-conversations --home "$HOME/.local/share/agent-conversations/helper" api POST /api/v1/conversations --body-file new-conversation.json
```

Configuration is editable as a JSON file or through `config get`, `config set --file …`, and the admin configuration API. Changes take effect after restarting this web service. Identity and owner are installation settings. Keep credential values out of command arguments and non-secret environment settings.

## Local and remote working folders

An ACP connection's `cwd` is the local folder in which Conversations launches
the configured command. Set optional `session_cwd` when the agent runs on another
machine: it is sent unchanged in ACP session creation and restoration. For
example, a Linux-hosted experience can launch its local attach executable while
requesting `/Users/owner/agent/workspace` on a Mac. No path translation or file
sharing is inferred. Client filesystem and terminal callbacks remain governed
by their separately configured local roots and audience policy.

