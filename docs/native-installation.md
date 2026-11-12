# Native Conversations and Companion packages

The native bundle is a self-contained hosting package for an existing ACP agent.
It contains the Conversations executable, a private Node runtime and the locked
production voice/MCP SDK dependencies. It does not install an ACP engine, model,
Minecraft server, developer toolchain, or a system-wide Node/Python installation.

Packages are separate for Windows x64/ARM64, macOS Intel/Apple silicon and Linux
x64/ARM64. Choose the actual machine architecture; Windows ARM64 is a native
package, not the x64 package running under emulation. The Linux baseline is
Ubuntu 24.04 x64/ARM64, matching the native build and isolated userland checks.
These packages do not claim compatibility with older glibc releases or arbitrary
Linux distributions; other distributions require their own qualification.

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
5. Follow the optional voice configuration below using the printed package paths.
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

## Sign in and open the interface

Use the actual operating-system account name. With no `--owner` argument,
initialization resolves the current OS account; it does not trust a shell's
`USER` variable. The generated certificate's CA is
`<installation folder>/tls/root-ca.crt`. Trust that CA in the browser/OS before
using the HTTPS address printed by `status` or launching `open`. Never expose
`tls/key.pem`, private credentials, or the installation folder over HTTP.

On supported Ubuntu Linux, the default service name is `agent-conversations`.
The package includes `ops/pam-agent-conversations`, containing the Ubuntu
`common-auth` and `common-account` includes. An administrator must review and
install that file as `/etc/pam.d/agent-conversations` before relying on password
sign-in. If a file already exists there, preserve it and review its policy; do
not overwrite an organization's authentication configuration. This is local OS
authentication setup, not a Rust/Python/Node dependency. Other Linux distributions
need a reviewed PAM policy appropriate to their system and their own runtime
qualification. A successful owner-token API test does not prove password sign-in.

Local account authentication is OS-specific: Linux uses the explicitly configured
PAM service, macOS uses OpenPAM, and Windows validates account credentials and
matches the authenticated account SID against the configured allowed accounts.
Windows Hello/PIN and browser passkeys are not password substitutes. Additional
allowed accounts retain their own room authorization; they do not gain the
installation owner's administrative rights merely by authenticating.

The `open` command opens this installation's existing HTTPS interface without
putting credentials in its URL. Start `serve` first. For optional automatic
startup, run the installed executable with `--home <folder> startup install`.
It registers this installation for the current user's sign-in and does not start
it immediately. `startup remove` removes only the matching owned registration.
Linux requires its user service manager; macOS uses a LaunchAgent and Windows
uses a task for the current account. Keep the installation and its immutable
package folders in place. Startup behavior requires a real sign-out/sign-in
check on the intended OS; service startup alone does not prove that check.

## Optional voice configuration

The package already contains Node and the locked voice SDK dependencies. Do not
run `npm ci`, `pip`, or a source-build guide on the destination computer.
For a new voice setup, copy `ops/companion.example.json` into the installation
folder as `companion.json`. Do not replace an existing `companion.json`.
The template is disabled, has no contacts or credentials, and performs no work.

Replace `node` and `bridge` with the **absolute installed** entrypoints printed by
`bundle install`, not the temporary extracted download paths. JSON strings must
escape Windows backslashes (`C:\\Users\\…`) or use forward slashes. In `profiles`,
add an existing saved agent ID and a chosen supported voice, for example:

```json
"profiles": {
  "default": {
    "voice": "marin",
    "description": "My existing ACP agent with OpenAI cloud voice",
    "context_registered": false
  }
}
```

Use the actual saved ID from this installation; the example does not create an
agent. Keep `context_registered` false unless this installation's transcript
provider is independently registered and reachable. Add no domain action or
observer configuration merely to test voice. Set `enabled` to true only when
intentionally enabling this voice connection.

Import the OpenAI key using the native `credential import-openai` command under
the same `--home` path. It reads the key from standard input and stores it in the
installation's protected `secrets/openai-api-key`; it does not take a key argument,
echo it, or contact OpenAI. Supply standard input from a protected file or a
secret manager; do not type the key into command arguments or shell history.
An existing `OPENAI_API_KEY` in the service environment takes precedence. A key
exported only in an interactive shell does **not** persist into login startup:
use the installation credential or explicitly configure the service environment.
The key is for the voice bridge and is not forwarded to the ACP agent.

First verify sign-in and the configured contact without starting a paid call.
Actual microphone, speaker, camera consent, and provider connectivity need a
separate end-to-end check in the intended browser. The package does not make
these tests, activate the camera, or initiate a call automatically.

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
