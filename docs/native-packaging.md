# Native bundle construction

`scripts/package-native.py` is a build/CI tool. End users launch the packaged
native executable; they do not install Python, Node, npm, Rust, or a compiler.
Packaging invokes none of those runtimes and performs no downloads, setup,
authentication, agent calls, or installation-home writes.

Build each explicit target separately: `windows-x64`, `windows-arm64`,
`macos-x64`, `macos-arm64`, `linux-x64`, and `linux-arm64`. Windows uses ZIP;
the other targets use tar.gz. The tool checks PE, thin Mach-O, or ELF headers
for the service and private Node executable. That proves the artifact's declared
machine identity, **not** that it launches or is compatible with the target OS.

## Supply frozen inputs

The release builder acquires and verifies the native service, an official Node
distribution, Node's license, and Rust third-party notices. Install the bridge's
production dependencies in a disposable build directory using its committed
`package-lock.json` and `npm ci --omit=dev --ignore-scripts`. This is build work,
never a first-launch or end-user step. Keep the lock and acquired-input provenance.

Inspect the already acquired bridge inputs:

```sh
python scripts/package-native.py digest \
  --bridge /absolute/source/bridge \
  --node-modules /absolute/build/node_modules
```

Provide a JSON input file with the following shape. Replace paths, version,
revision, and digests with the exact frozen build inputs. Do not use placeholder
digests or select dependencies at application launch.

```json
{
  "schema_version": 1,
  "target": "windows-arm64",
  "version": "0.12.8",
  "source_revision": "FULL_40_CHARACTER_REVIEWED_REVISION",
  "service": {"path": "/absolute/agent-conversations.exe", "sha256": "SHA256"},
  "node": {
    "executable": {"path": "/absolute/node.exe", "sha256": "SHA256"},
    "version": "22.23.5",
    "license": {"path": "/absolute/node/LICENSE", "sha256": "SHA256"}
  },
  "bridge": {
    "path": "/absolute/source/bridge",
    "source_sha256": "DIGEST_OUTPUT_SOURCE_SHA256",
    "node_modules": "/absolute/build/node_modules",
    "node_modules_sha256": "DIGEST_OUTPUT_NODE_MODULES_SHA256"
  },
  "license": {"path": "/absolute/source/LICENSE", "sha256": "SHA256"},
  "third_party_notices": {"path": "/absolute/build/RUST-NOTICES.txt", "sha256": "SHA256"},
  "support_files": {
    "docs/native-installation.md": {"path": "/absolute/source/docs/native-installation.md", "sha256": "SHA256"},
    "ops/pam-agent-conversations": {"path": "/absolute/source/ops/pam-agent-conversations", "sha256": "SHA256"},
    "ops/companion.example.json": {"path": "/absolute/source/ops/companion.example.json", "sha256": "SHA256"}
  }
}
```

```sh
python scripts/package-native.py build --inputs /absolute/inputs.json --output /absolute/dist
```

The archive root contains:

```text
agent-conversations[.exe]
manifest.json
runtime/node[.exe]
bridge/live.mjs
bridge/companion-mcp.mjs
bridge/<other existing bridge source files>
bridge/package.json
bridge/package-lock.json
bridge/node_modules/<locked production dependencies and their notices>
licenses/Conversations-LICENSE
licenses/Node-LICENSE
licenses/THIRD-PARTY-NOTICES.txt
licenses/npm-dependencies.json
docs/native-installation.md
ops/pam-agent-conversations
ops/companion.example.json
```

The generated package receipt names the archive and binds its SHA-256 plus the
manifest SHA-256. The manifest lists every payload file's relative path, digest,
size and executable bit, target identity, source revision, entrypoints, input
digests and Node version. The manifest does not hash itself; the receipt binds it.
Dependency provenance records each retained npm package's version, registry URL,
lock integrity, license metadata and license-file paths. Production package files
remain intact. Dev-only packages, npm command shims and build caches are excluded.
Linked/optional packages and native npm addons currently require explicit future
qualification instead of being silently included or omitted.

The bundle uses relative entrypoints. Its native installer/launcher resolves them
under the installed bundle directory and passes absolute installation-owned paths
to the existing service configuration. It does not discover a system Node or
change the selected agent engine. Optional Python domain helper source remains
available for separately owned connector installation; this package does not
claim an included Python interpreter or portable Minecraft installation.

The fixed support files are pinned package inputs, just like the executable and
licenses. They provide native-consumer setup instructions, the Ubuntu PAM policy
for an administrator to review, and a disabled credential-free voice template.
They are not applied to an installation or to `/etc` by packaging or extraction.

## Qualification layers

The unit suite uses tiny non-runnable binary headers and fake packages. It checks
all six identities, malformed/mismatched/incomplete inputs, pinned dependencies,
archive integrity, traversal/link rejection, deterministic output and relocation.
It is not native-platform qualification.

The clean-machine harness must verify the archive before extraction, reject
unlisted, duplicate, absolute, traversing and linked members, and materialize only
validated regular files into a new directory. Relocate it into a path containing
spaces and Unicode. Use a fresh ordinary-user home and a restricted child `PATH`
without developer tools, system Node or Python. Run the **packaged** service and
Node by their manifest paths, verify startup/authenticated APIs and a disposable
read-only standard ACP fixture, then stop and check descendant cleanup. No real
model, world, credentials or user session is needed for this layer. The build/CI
`inspect_archive` helper performs pre-extraction integrity checks; the native
installer owns the independent end-user implementation.

Real microphone/camera/playback, trusted HTTPS, recovery, signed Windows
distribution and Apple signing/notarization need separate platform evidence.
A successful compile or dependency-free smoke test establishes none of those.

The separate `tests/native_clean_userland.py` controller extends Linux launch
coverage to a checksum-bound official Ubuntu 24.04 container with no development
runtimes installed. CI must acquire the fixed image digest first; the harness
never pulls images or installs packages. It checks the native Docker engine/image
architecture, safely verifies/extracts the archive, mounts only package and fixture
readonly, and runs as UID 1000 with no network, no capabilities, a readonly root,
and disposable executable home. The package's private Node drives native install,
ACP initialize, bundled SDK imports and CA-verified HTTPS/API checks. Host Python
and Docker are CI tools, not application requirements. Only labeled owned
containers and temporary extraction are removed. The receipt binds artifact,
source, image and fixture hashes, including failed output and cleanup results.

```sh
python tests/native_clean_userland.py \
  --archive /absolute/dist/conversations-VERSION-linux-ARCH.tar.gz \
  --receipt /absolute/dist/conversations-VERSION-linux-ARCH.json \
  --output /absolute/results/clean-userland.json
```

This is native Linux **userland** evidence sharing the host kernel, not a fresh
desktop VM, physical browser/media test, PAM password check, or six-platform
consumer qualification. Ubuntu 24.04 is the explicit Linux baseline.
