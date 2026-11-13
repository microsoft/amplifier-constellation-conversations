# Agreements and interfaces

[Conversations](VISION.md) supplies reusable conversation and coordination
services. [Companion](companion-vision.md) presents them in a live experience with
an agent in the physical world. Their current implementations share this
repository. Products can use the conversation services independently.

The family's [five primitives](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/README.md)
describe composable roles, not required services or repositories. This component
is a source of [Conversations](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/conversation.md);
its reference clients and Companion are [Experiences](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/experience.md).
ACP attachments are [Connections](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/connection.md)
to independently configured [Agent Instances](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/agent-instance.md).
Its advertised [Tools](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/tool.md)
expose authorized service capabilities. An agent's internal architecture remains
its owner's choice; the exact supported ACP boundary is defined below.

Start with the relevant vision, then the agreement or local interface your work
changes. Setup instructions and examples illustrate these interfaces without
adding separate commitments.

## Shared agreements

Constellation owns the cross-component behavior in four shared agreements:

| Agreement | Responsibility |
| --- | --- |
| [Composition](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/composition.md) | Identity, audience, source ownership and independent composition. |
| [Interaction](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/interaction.md) | Human attention, inspectable collaboration and return of useful results. |
| [Work authority](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/work-authority.md) | Acceptance, changes, delegation and Stop. |
| [Work continuity](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/work-continuity.md) | Input during work, interruption and recovery. |

The scoped [installation agreement](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/installations.md)
covers setup, connections, updates and preserved state. The scoped
[engine compatibility agreement](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/engine-compatibility.md)
covers SDKs, negotiated capabilities and explicit engine/model choice.

These agreements remain authoritative across components. Concrete routes,
schemas, configuration and supported behavior remain with the serving owner.
Intended repository homes do not establish a new API or transfer an existing one.
Use [Converge](https://github.com/microsoft/amplifier-converge) when changing a
shared promise; ordinary choices within that promise remain with the owner.

## Conversations service interfaces

| Local promise | Exact definition |
| --- | --- |
| Rooms expose messages, membership, attachments and view state through authenticated operations. | [API contract](api.md), [room participation and access](shared-rooms.md). |
| Accepted messages retain their delivery identity and ordered path through steering or queued input. | [Input continuity](input-continuity.md). |
| Optional local filesystem and terminal callbacks use configured audience checks and operation-bound approvals. | [ACP callbacks](client-callbacks.md). |
| Huddles bind a work discussion to its original request, with attributed posts, inspection and result return. | [Huddle interface](huddles.md). |
| Authorized context queries expose this service's conversations without granting permission to send messages or administer the service. | [Context-provider interface](api.md#context-provider-interface). |

The linked specifications define these promises and their limits. Reference-client
instructions describe one way to use the same operations.

## Companion call interfaces

| Local promise | Exact definition |
| --- | --- |
| Call/media controls, consent and provider connections belong to the call experience, separate from backend ACP work. | [Call behavior and installation](companion.md). |
| Shared material, presentation controls and saved results retain their call and audience scope. | [Sharing and receipts](companion.md#sharing-capabilities-and-receipts), [call API](companion.md#api-reference). |
| Optional connected tools and observers preserve source attribution and distinguish submitted work from observed outcomes. | [Connected-experience tools](companion.md#optional-connected-experience-tools), [observers](companion.md#read-only-observers), [Minecraft observation bridge](companion-minecraft-observation.md). |

These are the current serving interfaces, not a claim that every product vision
scenario is implemented. Media transport is not a new ACP protocol; the technical
documents retain capability requirements and unsupported cases.

For personal setup, start with [Open Companion](native-consumer-setup.md): open
the native app and create your account in the browser, then choose an agent and
add any missing voice credentials. No local Instance or developer tools are
required. Existing administered installations can keep using
[Advanced native hosting](native-installation.md); source builds use
[Installation and connection configuration](installation.md).
[Validation](validation.md) describes reproducible
checks and their limits. [Release notes](releases/) retain historical changes;
they do not define another roadmap or set of current promises.
