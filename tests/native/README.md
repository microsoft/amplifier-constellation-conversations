# Native package qualification fixture

`fixture.mjs` is a model-free ACP agent using the official TypeScript SDK. Its
only successful request is `initialize`; every other request fails. The SDK is a
locked test dependency, copied beside the fixture in a disposable directory. It
is not added to the application bundle or required on an end-user machine.

The separate `native-platform.yml` workflow builds on six native runners and
then invokes `tests/native_clean_install.py`. The application runs from a moved
package, then an installed package, under a path containing spaces and Unicode.
Its environment has a new user home, no provider credentials or SDK search paths,
and a PATH containing no development tools. Python is the external CI controller,
not a child-process or installed-application dependency.

The fixture checks bundled Node execution and architecture, complete manifest
validation, the native installer, official ACP initialization, bundled bridge SDK
imports, HTTPS startup with the generated CA explicitly trusted by the test
client, authenticated local API access, denial of anonymous API access, and
shutdown. It creates no ACP session, model prompt, voice call or work assignment.

Each result states its limits. Hosted runners already contain development tools;
isolating the application PATH demonstrates that the tested path does not depend
on them, but is not a freshly provisioned consumer OS. Windows x64 runs on Windows
Server 2025 and needs a separate Windows 11 x64 qualification. Native Windows
ARM64 runs on `windows-11-arm`; x64 emulation does not satisfy that lane. OS
password authentication, certificate trust in a normal browser profile, physical
microphone/camera, login startup, signing and notarization require their own
receipts. Neither workflow dispatch nor an artifact upload publishes a release
or changes the paused speech release.

Both MSVC builds explicitly request the static C runtime for the target, with an
explicit Cargo target so host build tools retain their own configuration. The
build provenance records those flags and the exact executable hash. A pinned,
build-only `pefile` inspection records normal and delay-load imports for both the
service and private Node. Any Visual C++ redistributable or unreviewed DLL import
blocks packaging pending a documented deployment route. System-only imports do
not by themselves prove a clean Windows installation, and arbitrary dynamic
`LoadLibrary` use still requires behavioral qualification. These checks follow
the [Rust C runtime linkage guidance](https://doc.rust-lang.org/reference/linkage.html#static-and-dynamic-c-runtimes)
and [Microsoft redistribution guidance](https://learn.microsoft.com/en-us/cpp/windows/determining-which-dlls-to-redistribute).
