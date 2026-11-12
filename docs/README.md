# Agreements and interfaces

[Conversations](VISION.md) supplies reusable conversation and coordination
services. [Companion](companion-vision.md) is a product for live interaction with
an agent in the physical world. Their current implementations share this
repository. Products can use the conversation services without adopting Companion.

Start with the relevant vision, then the agreement or local interface your work
changes. Setup instructions and examples illustrate these interfaces without
adding separate commitments.

## Shared agreements

Constellation owns the cross-component promises in six agreements:

| Agreement | Responsibility |
| --- | --- |
| [Composition](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/composition.md) | Ownership and independent composition, including intended product and platform homes. |
| [Interaction](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/interaction.md) | Conversation, human attention and return of useful results. |
| [Work authority](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/work-authority.md) | Acceptance, changes, delegation and Stop. |
| [Work continuity](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/work-continuity.md) | Input during work, interruption and recovery. |
| [Installations](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/installations.md) | Setup, connections, updates and preservation of state. |
| [Engine compatibility](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/engine-compatibility.md) | SDKs, negotiated capabilities and explicit engine/model choice. |

These agreements remain authoritative across components. Concrete routes,
schemas, configuration and supported behavior remain with the serving owner.
Intended repository homes do not establish a new API or transfer an existing one.

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

For setup, use [Installation and connection configuration](installation.md) and
the relevant component's configuration above. [Validation](validation.md) describes reproducible
checks and their limits. [Release notes](releases/) retain historical changes;
they do not define another roadmap or set of current promises.
