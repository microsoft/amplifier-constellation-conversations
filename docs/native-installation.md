# Native Conversations and Companion packages

The native bundle is a self-contained hosting package for an existing ACP agent.
It contains the Conversations executable, a private Node runtime and the locked
production voice/MCP SDK dependencies. It does not install an ACP engine, model,
Minecraft server, developer toolchain, or a system-wide Node/Python installation.

Packages are separate for Windows x64/ARM64, macOS Intel/Apple silicon and Linux
x64/ARM64. Choose the actual machine architecture; Windows ARM64 is a native
package, not the x64 package running under emulation. Linux packages target the
supported distribution baseline used by their native build lane.

## Install without developer tools

1. Obtain the package and its checksum from the same trusted release. Verify the
   published checksum and publisher signature where provided, then extract it
   using the operating system's archive tools. The package's internal manifest
   checks integrity; it is not a substitute for authenticating its publisher.
2. Run the extracted `agent-conversations` (`agent-conversations.exe` on Windows)
   with `--home <absolute installation folder> bundle verify --path <extracted folder>`.
3. Run the same command with `bundle install` instead of `bundle verify`. The
   native executable copies the verified bundle into a private immutable package
   folder and prints its absolute entrypoints. It preserves existing settings,
   credentials, sessions and packages. It does not start or update a running
   service.
4. For a **new** installation, run the printed installed executable with
   `--home <folder> init --agent-home <existing Instance folder>` or supply the
   existing ACP command and arguments with `--agent-command` and `--agent-arg`.
   The agent remains independently installed and owned. Existing installations
   must retain their existing configuration instead of running `init` again.
5. Use the printed package paths for Companion's `node` and `bridge` settings.
   Keep each existing voice profile, connection and credential choice. A package
   install alone does not enable voice, domain actions or automatic updates.
6. Configure the installation's HTTPS certificate and allowed local accounts,
   then run the installed executable with `--home <folder> serve`. Use its HTTPS
   address in the system browser. Trust the installation's CA through the normal
   operating-system/browser flow, or provide an already trusted certificate at
   initialization. Do not disable certificate validation.

These commands use the packaged executable directly; no Cargo, npm, pip, Node,
Python, WSL, compiler, or build step is needed on the destination computer.
Developer tools used by CI or the package builder are not shipped prerequisites.
Paths containing spaces and Unicode must be passed as one command argument.

Local account authentication is OS-specific: Linux uses the explicitly configured
PAM service, macOS uses OpenPAM, and Windows validates account credentials and
matches the authenticated account SID against the configured allowed accounts.
Windows Hello/PIN and browser passkeys are not password substitutes. Additional
allowed accounts retain their own room authorization; they do not gain the
installation owner's administrative rights merely by authenticating.

## Qualification and current limits

The native workflow builds and exercises all six target packages independently.
Its clean-path harness relocates the package, creates a fresh application home,
removes developer tools from child process PATH, imports the private SDKs, starts
HTTPS with explicit CA validation, and initializes a read-only official-SDK ACP
fixture. It creates no model prompt or game action. Each run emits its actual
OS/architecture, source and package hashes, checks and untested boundaries.

A hosted runner with an isolated application environment is **not** a newly
provisioned consumer computer. In particular the Windows x64 hosted lane uses
Windows Server, while Windows ARM uses Windows 11. Fresh Windows 11 x64 testing,
OS account sign-in, browser microphone/camera permissions and physical media,
startup registration, signed installation/SmartScreen/Gatekeeper and upgrade/
rollback require separate platform receipts. Passing build or manifest tests
must not be reported as passing those checks.

Optional filesystem callbacks remain explicitly constrained by platform support;
terminal callbacks require the existing Linux confinement. A native voice package
does not silently grant unrestricted terminal or filesystem access. Optional
Minecraft/other domain helpers keep their independent installation ownership;
there is no assumption that system Python or Node is available for them.

The native package work is separate from the paused speech release. Building or
qualifying an artifact does not resume that release, publish it, or adopt it on an
existing installation.
