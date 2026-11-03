# Shared conversations (0.3)

Agent Conversations owns its browser conversations. An ACP connection identifies an existing independently running agent or an attach command; the service does not install agents, read their private state, or share Slack storage. The same endpoint may also be attached to Slack, Zed, or a fleet experience.

## Start simply

New conversations contain the signed-in person and the default agent. They are private. Existing conversations and imported Zed/Toad transcripts remain private on upgrade. Imported copies cannot be shared or prompted here.

Choose **People & agents** in a native conversation to add participants. Adding a person or another agent requires checking **Share this conversation’s existing messages and files**. This grants access to the existing room history. Start a new conversation instead when that is not intended. The room owner manages participants and its title. Members can read, post, attach files, and stop active work.

A person must have an existing local account allowed by the service configuration:

```json
{"owner":"operator","allowed_users":["robin","casey"]}
```

These are fields in the full installation config, not a replacement for it. Add accounts through your normal host administration; the service never creates OS accounts. `config set --file` or `PUT /api/v1/config` saves a complete configuration for restart. PAM checks each allowed person's own credentials. Allowing a user permits them to talk to the configured ACP endpoints, whose own tool and credential policies still apply. Do not allow people to use an endpoint whose authority they should not exercise.

## ACP connections

The default connection still comes from `config.agent`. Additional connections are stored independently in this experience's private SQLite store and survive restart. Only the installation owner can register/update connections:

```json
{
  "id":"reviewer",
  "config":{
    "label":"Reviewer",
    "command":"ssh",
    "args":["agent-host","/path/to/an-acp-attach-command"],
    "cwd":"/home/operator",
    "env":{},
    "mcp_servers":[],
    "timeout_seconds":900
  }
}
```

Send that to `POST /api/v1/agents`, or use `agent-conversations --home … api POST /api/v1/agents --body-file connection.json`. Reusing an ID updates that connection. Commands and arguments are executed without a shell. Use existing SSH authentication or private configuration for credentials. Updating an endpoint resets its working sessions; the agent's runtime is untouched. Existing attached connections cannot be deleted until removed from their rooms.

The UI puts connection setup in an advanced section. Browser view state accepts non-secret connection drafts; environment values, MCP credentials, passwords and tokens belong in protected operator configuration, not drafts. Ordinary members receive labels and IDs only, never endpoint commands or MCP/environment settings.

## Conversation and participation

Each `(conversation, agent connection)` has its own resumable ACP session and public-message cursor. No session is reused across rooms or between agents. Changing an audience revokes source grants and resets working sessions. Sharing a formerly private conversation never carries its previous private ACP working context into the room.

Choose **Everyone here** to give each agent a bounded opportunity to contribute. In a group, an agent may return `[[PASS]]`; this records a listening decision in Activity without posting a blank message. Contributions are considered sequentially, so later agents see replies already posted. Choose a named agent to address it directly; other agents receive no prompt for that turn. A one-person/one-agent conversation expects a response. These behaviors are ordinary ACP prompt guidance, not a proprietary protocol extension or a guarantee about a model's judgment.

This version serializes work within each room, with up to four rooms active per installation. Stop active work before posting another message in the same room. It does not run an unbounded agent-to-agent loop or claim to coordinate concurrent human interruptions. A room can have up to 50 people and eight agents. Buffered group replies become public only when complete. Failed/cancelled working sessions are rebuilt from actual completed room messages, so private drafts are not mistaken for delivered replies. Removal of a person immediately revokes room access and cancels current work; adding participants waits for the current turn to finish.

## Attachments and history

A message can include up to four private attachments. Supported formats:

- PNG, JPEG, GIF, WebP: at most 2 MB each, supplied as ACP image blocks only if the endpoint advertises image input.
- UTF-8 plain text, Markdown, CSV, JSON: at most 200 KB each; embedded resources when supported, otherwise explicit document text blocks.

Unsupported image input produces a clear limitation in the agent's context. The adapter never says an image was read when it was not supplied. PDF files up to 2 MB are sent as standard ACP embedded resources when the endpoint advertises embedded context; its provider must also support PDF input. Otherwise the prompt states that the document was not supplied. Other file types, including audio/video inputs, are rejected with an explanation. Downloads require current room membership and use attachment disposition with `nosniff`; uploaded HTML/SVG and arbitrary executable files are not accepted. Files do not become host filesystem paths. Unsent uploads remain visible only to their uploader. Current-turn attachments are supplied to the addressed agents; earlier messages retain metadata and authenticated download links for people, rather than silently reattaching all old files to every prompt.

Rooms have a 64 MB attachment budget and at most 20 unsent uploads. A user can delete their unsent uploads. Deleting a conversation deletes its stored files; it cannot erase content already incorporated into an agent's own memory.

The transcript opens on the latest 100 messages. **Find in this conversation**, **Earlier messages**, and **Back to latest** use the same authenticated cursor APIs as tools. Search in the sidebar matches accessible titles or message text. Context-provider `read`/`search` also supports bounded historical paging and source attribution.

## Audience boundary

PAM sessions are identities, not installation-wide read access. Every native conversation route, attachment download, activity read, and view operation checks membership or user ownership. An installation control token represents the configured owner; it does not automatically enter other users' private conversations. Views and unsent drafts belong to their signed-in human. Removing a person deletes their room-specific view and causes subsequent reads and downloads to fail.

New context credentials identify one registered agent and only read rooms that agent currently belongs to, intersected with the human/grant scope. Removal also blocks retrieval through prior grants. The legacy default-agent credential now applies the same membership check to the default agent, with its existing and future imported histories explicitly assigned to it. These source credentials remain machine capabilities. Owner-private conversation grants preserve authorized owner continuity. Shared rooms and any other user's private room receive room-only grants with `authorize.owner:false`; attempting to read another room with one fails. Grant scope can narrow but never expand when a room is shared. Audience changes revoke outstanding grants. Native provider results contain actual participant attribution. Imported source provenance and independently scoped source credentials are preserved.

## Programmatic UI

All actions use the public authenticated API. View state exposes selected conversation, participant panel, invite selections, history-sharing checkbox, reply targets, staged attachment IDs, text draft, search/cursor, advanced connection draft/expansion, focus, scroll, mobile navigation, and rename dialog. It contains no grant or connection credentials. Upload and send remain distinct explicit operations; changing a view cannot send a message or invite someone.

This service remains standalone. A fleet experience can list/configure ACP connections and link to room URLs without knowing how an agent is built. Cross-origin iframe embedding is not enabled by default; keep the same-origin policy or deliberately configure a separate trusted embedding boundary in a future revision.

## Verification

`cargo test --all-targets` exercises authenticated audience isolation, room grants, explicit sharing, independent ACP sessions, ambient/direct participation, requester-bound approval decisions and cancellation, attachment bounds/capability fallback, historical pagination/search, restart, imports, and view ownership. `tests/browser.py` runs the real HTTPS service with model-free ACP subprocesses and two independently authenticated browser contexts. It checks the existing private experience plus invited-user visibility, attributed messages, participant controls, attachments, targeting, history control, removal, mobile layout, and cancellation. Disposable browser sessions do not claim to validate a real user's PAM password or real-model social judgment.

## Agent sign-in and external steps

An agent that returns ACP `AUTH_REQUIRED` during new/load setup can offer its
`authMethods`. Conversations asks the initiating person to choose a supported
agent method, calls `authenticate`, and retries session setup once. It never
replays a model prompt to recover authentication. Terminal-only login methods
remain a host setup task; their commands are not executed by the web service.

ACP form and URL elicitations are private to the initiating person, including
inside a shared room. External steps show the full address and host and require
an explicit **Open and continue** action. Accepting the URL means consent to open
it, not proof that login completed; the agent must complete its authentication
request. Credentials are entered at the provider. URLs and answers are not
written into the conversation or activity audit. Unanswered requests expire and
are removed on cancellation. Only HTTPS (or loopback HTTP) destinations are
accepted. The same decisions are available through the elicitation API and view
controls; `elicitation_open_url` is the browser action for an external step.
