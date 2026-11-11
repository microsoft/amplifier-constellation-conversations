# Conversations and Companion documentation

[The component vision](VISION.md) explains the intent of these services and their
reference clients. This page locates the agreements and interfaces; it does not
add another specification or prescribe a product UI.

## Shared agreements

The Constellation index owns the family-wide intent and commitments:

- [Constellation vision](https://github.com/microsoft/amplifier-constellation/blob/main/docs/VISION.md): what the family is working toward.
- [Family composition and ownership](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/family.v1-candidate.md): service responsibilities, information ownership, interfaces and self-contained installations.
- [Interaction across services and experiences](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/experience.v1-candidate.md): identity, audience, authority, continuity and observable outcomes across experiences.

Those agreements apply across the family. The documents below own this
component's concrete interfaces and behavior. Setup instructions and examples
illustrate those interfaces without adding separate commitments.

## Local interfaces and behavior

| Document | What it defines |
| --- | --- |
| [API contract](api.md) | Authentication, rooms, messages, membership, attachments, imports, view state, source context, approvals and forms. |
| [Shared conversations](shared-rooms.md) | Registered ACP connections, participant and history access, attachments, and reference-client operations. |
| [Input continuity](input-continuity.md) | Ordered input, negotiated steering, delivery states, explicit Stop and uncertain-outcome recovery. |
| [Optional ACP callbacks](client-callbacks.md) | Configured local filesystem and terminal access, audience checks, permissions and platform boundaries. |
| [Observable huddles](huddles.md) | Work-room setup, participant enrollment, attributed posts and returns to the original request. |
| [Companion](companion.md) | Call and media lifecycle, voice-provider configuration, scoped tools, camera, shared visual surface, observers and reference-client controls. |
| [Companion's Minecraft observation bridge](companion-minecraft-observation.md) | Domain-owned source and action evidence exposed to a call, including outcome and audience limits. |

For setup, start with [Build and install](../README.md#build-and-install), then
the interface for the capability being configured. The browser screens provide
developer, administrator and reference access; their layout is not the intended
product experience for every future client.

[Validation](validation.md) describes reproducible checks and their limits.
[Release notes](releases/) retain historical changes; they are not a separate
source of current interface promises or a roadmap.
