# Open Companion

A personal Companion installation can start with no agent connected. It owns
its account, contacts, voice credentials and update state on this computer.
It does not require a local Instance, shared enrollment service or developer tools.

1. Download the package for your operating system and architecture from a trusted
   release. Check its published checksum and signing information before opening it.
2. Extract the complete download. On macOS, open `Companion.app`; on Windows,
   open `agent-conversations.exe`; on Linux, open the packaged `agent-conversations`
   executable. Keep the bundled files together. No terminal arguments are needed.
3. The app opens Companion in your browser. Choose a local Companion password.
   This is separate from your computer password. Optional sign-in startup uses
   the current OS account and the existing installation startup registration.
4. Select a saved agent connection, or connect an existing Instance folder through
   **Add an agent**. The connection check initializes ACP only; it does not create
   an agent session or prompt a model. Other independently configured ACP commands
   remain supported by the advanced connection controls.
5. Choose a voice and supply an OpenAI API key only if the installation does not
   already provide one. The key is written privately on this computer and is not
   returned to the browser. An explicitly configured environment key takes
   precedence, including an empty one. Saving setup does not verify paid provider
   access, request media permissions or start a call.
6. Choose **Call** when ready. Microphone and camera permissions remain part of
   the existing explicit call experience.

Reopening the app uses the same saved identity and selected installed bundle.
Opening an older extracted download does not downgrade the installation. Selecting
an existing contact again preserves its endpoint, profile, grants and room sessions.

Fresh desktop installs listen only on a literal loopback address using local HTTP,
with exact saved Host/Origin checks. Local HTTP is not a LAN mode. The browser
account and private first-account capability are scoped to this installation.
The capability expires and is removed from the browser URL immediately. If setup
is interrupted, reopen the app; an existing password is never silently replaced.

Existing OS-account/HTTPS installations retain their settings and access policy.
The advanced [native hosting instructions](native-installation.md) remain available
for network access and independently administered accounts. Switching authentication
mode is not an automatic migration.

## Updates and recovery

Both desktop and advanced installations use the same
[whole-bundle update lifecycle](native-updates.md): immutable application/private
runtime/support bundles, admission deferral for calls and accepted or uncertain work,
verified rollback, and restoration of the original startup state. Passwords,
credentials, contacts, transcripts and work records are not rollback snapshots.
An enrolled installation has one updater owner; opening the app creates no
additional scheduler. Standalone use does not require enrollment.

Download and extract the complete newer release, then open its app. It opens an
update screen for the existing installation and shows the offered version. Sign in
with the existing Companion account if needed, then choose **Update** once.
The download can supply this restricted update screen even when the installed
version predates it. It does not open application work, migrate the database, or
replace the installed app merely because the download was opened.

The request is saved before package preparation starts. The screen shows progress
while the complete bundle is verified and staged in the background. Companion
continues running during preparation and waits for active calls and work to finish.
Uncertain earlier work requires attention; the updater does not discard or replay
it. Installation uses the existing independent native worker, validates the
prepared bytes again, and verifies the replacement before reporting success.

You can leave the page and return to the same saved request. Closing a browser or
losing a reply does not cancel or resubmit an update. **Cancel** is available before
the installation stop boundary; already-started file work finishes before
cancellation is confirmed. If the preparation process exits unexpectedly,
**Continue** can start a new verified preparation attempt only after its previous
ownership is conclusively released. An uncertain native launch requires explicit
recovery, never an automatic second launch.

If installation fails, the existing lifecycle restores the previous package where
it can prove that recovery is safe. The page distinguishes a verified rollback
from an unconfirmed outcome. Reopen the downloaded app or the installed app to
observe the retained operation and use its available recovery controls. Recovery
uses the existing account without opening calls, creating agent sessions or
resuming uncertain work. Restoring an earlier app package keeps the current
account, contacts, conversations and work records. Earlier update receipts remain
available after later updates.

**App updates** remains available below the contacts. Advanced controls can select
an extracted package on the installation computer. The separate CHECK/APPLY API
remains for existing advanced clients; it is not the ordinary browser workflow.
An enrolled installation directs you to its existing updater instead of creating
a second owner or scheduler.

## Qualification limits

Native CI distinguishes architecture builds, application PATH without developer
tools, actual local account/contact setup, registered OS startup, and whole-bundle
update/rollback. macOS application launch uses the ordinary LaunchServices entry
point in a disposable CI account. This is separate from testing a freshly installed
consumer OS, password sign-in after a reboot, physical microphone/camera behavior,
and publisher signing or notarization. Those results require their own receipts;
package integrity alone does not establish them. The paused speech variant is not
activated by native packaging or setup.
