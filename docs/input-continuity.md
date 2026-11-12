# Continue a conversation while an agent works

The composer stays available during work. Send a correction or an additional
constraint normally; **Stop** is a separate control. Sending does not cancel the
active prompt, duplicate its completed tool effects, or dismiss its pending
approval. Approval remains with the person whose turn requested it.

## Delivery and compatibility

Before returning HTTP 202, Conversations saves the source message, its UUID,
attachments, intended agent recipients and arrival order in one transaction.
Repeating the same UUID and payload returns the existing acknowledgment; equal
text with a new UUID is a new message. The browser keeps the UUID with its draft
until it receives acknowledgment, including across reload after a lost response.

For an endpoint advertising `_meta.steering.supported`, the active connection uses
`_session/steering` with `idleBehavior: promptRequired` and a per-recipient input
ID. One request at a time preserves ordering while further messages wait locally.
Before queued input is delivered, its original sign-in or token must still be
authorized and its author must remain in the room. These credential references
are stored privately and never sent to the agent or transcript. A definitive
idle result returns the source to the ordinary next-prompt path;
there is no detached task. Ambient input still goes through advertised Instance
attention. A deferred attention admission is reused only when the endpoint
explicitly reports its ticket restored; source, session and audience remain
bound to the original input.

Ordinary ACP endpoints need no extensions: messages wait until their current
prompt finishes. The browser cannot insert input between internal model calls
of an opaque agent. Each `(room, agent connection)` retains its own session,
queue and delivered cursor. A message targeting another agent or another room
never enters the current prompt. Per-room agent execution remains sequential.

After a successful prompt, its response and delivery are final and visible before
the client closes the agent's input and drains output for at most three seconds.
This terminal cleanup does not dispatch callbacks, append to the completed answer
or admit new work. The same room waits for cleanup before its next prompt, while
other rooms can use the released execution capacity. Stop ends the wait without
changing the accepted response or replaying it.

Output EOF, cleanup failure, Stop or the deadline ends this best-effort opportunity:
Conversations disposes the still-owned process group and reaps the child. EOF is
not proof of process exit or graceful agent retirement; an agent that closes
stdout early gets no further execution grace. Auxiliary work is not guaranteed
to finish. Failure and interrupted-prompt disposal otherwise remain unchanged.

The queue is bounded to 32 unresolved source messages and 8 MiB of source text
and attachments per room. HTTP 429 means the new message was not accepted. No
accepted source is silently discarded. Each ordinary next prompt takes its own
place among waiting rooms; updates within an active turn retain its deadline and
do not grant additional model/tool allowance.

## What the labels mean

- **Received**: saved by this experience, awaiting delivery.
- **Sent; acceptance pending**: transmission started.
- **Received by agent / Queued by agent**: the endpoint acknowledged intake.
- **Included in model context**: the endpoint supplied an incorporation receipt;
  this does not prove the model followed the instruction.
- **Turn finished**: the owning prompt returned normally. This alone is not an
  incorporation or task-success claim.
- **Queued for next turn**: the endpoint definitively returned an idle outcome.
- **Agent chose to listen**: attention declined a contribution.
- **Not accepted / Outcome unknown**: delivery needs review before this agent
  processes more messages in the room.

Receipts are per recipient and appear under the original human message. The
successful end of a prompt does not promote a received-only steering receipt to
incorporation. Such an input retains an unknown outcome and the no-replay fence;
an explicit incorporation receipt and target completion remain separate facts.
The original prompt input can settle from its own matching terminal response.

The conversation API and `/deliveries` expose the same states, source ID, target
turn, session, arrival sequence and bounded receipt metadata. Raw prompts,
attention tickets and credentials are not copied into delivery metadata. The
latest 256 history rows stay bounded, with every unresolved delivery retained
alongside them. An older unresolved message has a review link that opens its
source through the ordinary paginated history view.

## Recovery and explicit Stop

A lost transport or process restart retains the source and saved session. It
never automatically replays an uncertain message. Newer messages may still be
saved, but that recipient waits for review. Other recipients keep independent
progress. A failed load preserves the original session binding instead of
rebuilding its work from old messages.

After inspecting prior work, the message author can choose **Continue without
resending**. The authenticated resolution API uses
`{"disposition":"proceed_without_replay"}`. It preserves the original receipt
and excludes that source from later reconstructed context. Where the endpoint
advertises receipt queries and recovery acknowledgments, Conversations uses
those controls immediately in the same loaded session, even without a newer
message. Recovery itself issues no prompt. A review failure remains visible
and does not silently unblock later work. The button
never executes the original message again. Restoring the original endpoint and
session is required for recovery of steered input; no session is silently
substituted. An operator may need to repair a broken connection first.

**Stop** cancels the active client process and unsent queued messages. Already
performed work is not undone. A transmitted input whose final outcome is
uncertain remains visible for review. Source revocation and room removal retain
their existing authorization/cancellation behavior. Rejected, held, cancelled,
or reconciled input is never turned into a new live instruction by recovery.
