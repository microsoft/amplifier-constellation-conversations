# Agent Conversations

Reusable conversation and coordination services for people and **existing ACP
agents**, part of [Amplifier Constellation](https://github.com/microsoft/amplifier-constellation).

Conversations supplies rooms, membership, messages, authorized source context and
observable work huddles. Experience builders use these services through their
APIs and tools. Current browser screens provide developer, administrator and
reference access; products choose their own interaction.

This repository also contains the current Companion call implementation.
Companion is a distinct product for bringing an agent into the physical world
through live talk, seeing and sharing. Its vision is separate from the services
it uses and from the current preview's layout.

- [Conversations vision](docs/VISION.md): reusable conversation and coordination services.
- [Companion vision](docs/companion-vision.md): live interaction in the physical world.
- [Agreements and interfaces](docs/README.md): shared promises and precise local contracts.
- [API reference](docs/api.md): resources, supported operations and view control.

The service supports explicitly allowed local users, private conversations, and shared rooms with several independently configured ACP agents. Existing conversations stay private on upgrade. See [shared rooms](docs/shared-rooms.md) for setup, participation, attachments, and audience boundaries. Multiple conversations can run concurrently against the configured endpoints. Imported transcripts from other experiences are viewable here, with their original surface identified, but remain read-only.

[Observable huddles](docs/huddles.md) keep routine agent coordination available
for inspection while meaningful results return to the original request. External
participants keep their existing sessions. With installation-owned connections
configured, agents create or reuse a huddle and invite known peers through their
existing tools; people do not prepare rooms. Inspection and follow preferences
do not grant work authority or start another agent.

## Companion

The [current call interface](docs/companion.md) provides full-duplex GPT-Live
calls to existing ACP agents, with per-agent voices and background work. Its
`/companion` preview and optional server-side bridge exercise those capabilities.
Companion owns its voice-provider connection; the selected ACP engine owns backend
reasoning-provider calls. Existing text conversations and browser dictation keep
their current behavior.

## Setup and technical references

- [Open Companion](docs/native-consumer-setup.md): open the native app and complete personal account, agent-contact and voice setup in the browser, without a local Instance or developer tools.
- [Advanced native hosting](docs/native-installation.md): OS-account authentication, HTTPS and independently administered installations.
- [Source installation and connection configuration](docs/installation.md): build, authentication, ACP endpoints and source credentials.
- [Reference browser controls](docs/shared-rooms.md#reference-browser-phone-and-voice): text, voice, forms and shared-room presentation.
- [Validation](docs/validation.md): reproducible checks and their limits.

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
