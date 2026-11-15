# Existing Linux Conversations installations

An installation created before native bundles does not have a native rollback
package. `native-update bootstrap` continues to refuse a different installed
executable. The explicit legacy adoption path retains the old executable and
its existing service definition as a separate origin. It does not create a
second service or change sign-in startup preferences.

This path is limited to the qualified Conversations 0.9 schema on Linux.
Companion installations and homes with an enrolled updater are excluded.
Use the executable from the exact qualified successor bundle. Do not use a
different version's helper, rewrite configuration, or manufacture an old
native manifest.

## Ownership and maintenance boundary

Before maintenance, the installation owner must inspect current work and
external effects. Historical unknown outcomes remain unknown and must never
be replayed. They are not, by themselves, evidence of an active local process.
Pending input, active work, unsettled external effects and newly accepted work
must be distinguished from those historical records. The old server has no
lossless installation-wide drain API; cancellation and credential revocation
are not maintenance controls.

1. From the qualified package, capture the original running service:

   ```sh
   agent-conversations --home /absolute/existing/home native-update capture-legacy TRANSACTION_UUID --legacy-sha256 QUALIFIED_OLD_EXECUTABLE_SHA256
   ```

   The digest comes from independently qualified old bytes. Capture checks the
   running image, original process generations, service definition and policy,
   recursive control-group membership and the process-owned listener. It does
   not stop anything. The capture is create-only and belongs to that UUID.

2. The maintenance owner stops that exact original service. The adoption
   command does not issue this Stop or infer it from elapsed time. Recheck late
   accepted work at the stopped boundary.

3. Run the same qualified executable against its complete bundle:

   ```sh
   agent-conversations --home /absolute/existing/home native-update adopt-existing TRANSACTION_UUID --package /absolute/qualified/bundle
   ```

   Adoption retains the installation and service locks, proves original
   generations absent, the captured control group empty and listener refused,
   and audits raw SQLite state before startup can normalize anything. Its
   writer reservation becomes a durable closed admission gate atomically.
   Controlled initialization does not run a router, ACP driver or model.

   The candidate starts through the existing exact service. Its package,
   process entry, authenticated health, saved-state continuity and closed work
   admission are verified before the first genuine native receipt is committed.
   Only then is the gate released. The existing custom service definition is
   retained for subsequent ordinary native updates.

## Failure and recovery

An interrupted command is not permission to repeat it with another UUID.
`native-update status` reports the retained adoption phase. Recover the same
transaction using its qualified package helper:

```sh
agent-conversations --home /absolute/existing/home native-update recover-adoption TRANSACTION_UUID
```

Before native commitment, recovery can revoke and stop only the exact accepted
candidate generation. Exit, listener refusal and the service lock must be
proved before exchanging back. The old executable reopens the **current**
database; no saved database is copied back. Previous authenticated health and
original service policy are independently verified. This downgrade is limited
to the qualified pre-admission migration; after new work is admitted, future
updates use native packages.

Missing or changed identities, a changed profile, uncertain Start/Stop
acknowledgements, partial unsupported migration, changed original data or lost
finish acknowledgement remain explicit recovery states. No automatic Start,
Stop, message, input or task replay follows uncertainty. A committed gate that
was released despite a lost reply can be reconciled by reading its exact
transaction row.

The implementation does not claim atomicity against a hostile writer running
under the same OS account, infer external effect completion from local process
absence, or treat cleanup as application health. Preserve failed transaction
evidence for independent review.
