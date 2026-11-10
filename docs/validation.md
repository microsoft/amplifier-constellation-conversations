# Validation

Run the repository's automated checks from a clean checkout. The source fixtures
use fictional identities and temporary state. They do not contain installed
agents, personal profiles, account credentials, or exported conversations.

```sh
cargo fmt --check
cargo test --locked
cargo build --locked
node tests/voice.mjs
npm ci
npx playwright install chromium
python3 tests/browser.py
BROWSER_STEERING=1 python3 tests/browser.py
node --test tests/huddles-api.test.mjs
node tests/huddles-browser.mjs
BROWSER_HUDDLES=1 python3 tests/browser.py
cargo clippy --locked --all-targets -- -D warnings
```

See the test scripts and CI workflow for prerequisites and platform conditions.
Browser tests use disposable local services and require Playwright/Chromium.
Linux service tests may require PAM development headers; isolated command tests
also need the documented OS isolation prerequisites. Opt-in real account, model,
or messaging tests remain separate from deterministic checks.

The [Actions page](https://github.com/microsoft/amplifier-constellation-conversations/actions) records checks
against the published source. Earlier private deployment reports and their
transcripts are excluded. A passing fixture verifies its tested boundary, not
general agent quality, human approval, or live tenant compatibility.

The huddle browser journey uses the actual compiled HTTPS service, cookie/CSRF
authentication, SQLite and browser UI. It checks quiet peer posts, human
inspection/join, follow/quiet, a committed-but-lost HTTP acknowledgment across
reload, one result in the original conversation and Stop at phone width. It
asserts no ACP process is invoked. Companion's separate fixtures cover active
call layout and floor/result handling; actual model, audio and Minecraft-world
trials remain independently scoped.

`tests/steering.rs` exercises real duplex ACP subprocesses: repeated ordered input,
source deduplication, fragmented frames, unchanged approval and one effect,
incorporation before acknowledgment, generic FIFO, terminal/idle races, restored
ambient admission, independent agent queues, bounded capacity, explicit Stop,
restart/unknown recovery without replay, and failed session restoration.
The steering browser suite runs the HTTPS app through Chromium and checks the
working composer, a lost send acknowledgment across reload, unchanged approval,
truthful delivery labels, explicit recovery by both UI and programmatic control,
and mobile layout. These fixtures do not call a model or exercise live services.
