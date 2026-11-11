# Conversations vision

Conversations should make it easier to build experiences in which people and
agents can talk, coordinate work, and return useful results without asking a
person to follow every exchange. Its contribution to
[Constellation's vision](https://github.com/microsoft/amplifier-constellation/blob/main/docs/VISION.md)
is a set of reusable conversation services. Product owners can design the
interaction that fits their setting.

## Contribution to the family

This component owns its rooms, membership, messages, source access, and
coordination services. An experience can use a private conversation, shared room,
or observable huddle with independently configured agents. People should be able
to understand the work, inspect its supporting discussion, and contribute when
their judgment matters. Huddles give routine coordination a place to happen while
meaningful results return through the original interaction.

Conversations connects to existing agents through ACP. The endpoint supplies the
agent's identity and reasoning capabilities; Conversations supplies the conversation
and its authorized context. External platforms keep their own conversations and
domain controls. The [family agreement](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/family.v1-candidate.md)
defines those ownership boundaries, and the
[interaction agreement](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/experience.v1-candidate.md)
defines the shared meaning consumers can rely on. Concrete operations and failure
behavior belong in this component's [API reference](api.md),
[shared-room interface](shared-rooms.md), and [huddle interface](huddles.md).

## Leave experience design open

The packaged browser UI and Companion preview are developer, administration, and
reference clients. They help exercise service behavior and explore interactions;
their navigation, controls, and visual presentation do not establish the intended
UX for future products. Web, native, voice, and platform experiences can compose
the services they need through the supported interfaces.

[Companion](companion.md) explores conversation while work continues in the
background. People could talk with an agent away from a keyboard, show it something
through a camera, and use a small shared visual surface to work through it
together. These prototypes help test the services; they do not bound future
ambition or investment in the experience.

Companion's voice or media provider connection belongs to the experience.
The selected ACP engine separately owns backend reasoning-provider calls, tool
execution, and model-session history. A voice interface can change without
turning its transport into ACP or choosing a different reasoning engine for the
agent. Capability and authorization checks remain at the interfaces that serve
the operation.

## Judge the composed experience

A useful result is more than a message appearing in a room. Evaluate a complete
journey with the consuming experience: setup, conversation, work elsewhere,
correction, result delivery, and recovery when a receipt is uncertain. Include
the effort required from the person using it and the person operating it. The
shared agreements define the distinctions between a receipt, execution,
delivery, and verified completion; the component's implementation and evidence
must support its claims.

We use [Converge](https://github.com/microsoft/amplifier-converge) and the family's
[working practice](https://github.com/microsoft/amplifier-constellation/blob/main/docs/working-together.md)
to carry this direction into contributions. Inspect current source and the
relevant journey when choosing the next outcome. Keep detailed service behavior
with its implementation, and coordinate changed promises with affected owners
without making every consumer follow the whole system.
