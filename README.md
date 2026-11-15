# Agent Conversations

Conversations provides shared rooms, messages, and coordination for people and
existing ACP agents. An application can use its APIs to bring several independently
configured agents into a conversation while each agent keeps its own execution
sessions.

This repository also contains **Companion**, a browser experience for talking
with an agent by voice and sharing what you see. Both are part of
[Amplifier Constellation](https://github.com/microsoft/amplifier-constellation).

## Try Companion

Follow [Open Companion](docs/native-consumer-setup.md) for a personal installation:

1. Download and verify a trusted package for your computer, then extract all its files.
2. Open `Companion.app` on macOS, `agent-conversations.exe` on Windows, or
   `agent-conversations` on Linux. The app opens in your browser.
3. Create a local Companion password and select or add an existing agent connection.
4. Configure a voice provider if you want to make calls, then choose **Call** when ready.

You can complete setup without an agent connected. Saving setup does not start a
call or prompt a model. Microphone and camera access require your browser's
permission. Personal setup listens only on this computer; use
[advanced hosting](docs/native-installation.md) for HTTPS and network access.

The [setup guide](docs/native-consumer-setup.md) also covers updates and recovery.
An interrupted package check may still be running; check saved status before
starting another request.

## Build a conversation experience

Use the [source installation guide](docs/installation.md) to configure the service
and its ACP connections, then the [API reference](docs/api.md) to integrate it.

- [Shared rooms](docs/shared-rooms.md): membership, participation, attachments, and audience boundaries.
- [Huddles](docs/huddles.md): inspectable agent coordination that returns results to the original request.
- [Agreements and interfaces](docs/README.md): local contracts and the wider family boundaries.

Existing conversations remain private on upgrade. Imported transcripts retain
their original surface and are read-only. The bundled browser controls provide
reference and administration access; applications can present their own experience.

For product direction, see the [Conversations vision](docs/VISION.md) and
[Companion vision](docs/companion-vision.md). Contributors can use the
[validation guide](docs/validation.md).

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
