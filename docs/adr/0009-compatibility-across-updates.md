# Compatibility across updates

An unattended tuning session spans reboots, and the installed shycler build can change between them. Before 1.0 a changed tuning strategy or journal format may break an existing session, but the operator must see a clear refusal naming the build that wrote it and the way to archive it.

## Decision

`session.start` records the build's version, revision, ruleset, schema and fixes. Every `config.loaded` records the build that resumed the session. A change to the hardcoded ruleset is breaking; a change to configurable defaults or a fix that records facts more accurately or changes decisions is not. The journal schema is bumped only when this build cannot read an older journal the same way. New event kinds and new fields on existing kinds do not bump the schema; existing kinds carry the stamps so older builds can still read them.

`run`, `regain` and `reset --core` refuse a different ruleset or schema before appending. Read-only commands warn about a different ruleset and refuse a different schema. `reset --all` can archive either. The refusal is the deliberate exception to recording every action and decision as an event: writing an event might make the journal unreadable by the build that can continue the session. The reason goes to stderr, and thus to the system journal in a tuning boot. In a tuning boot, a refusal clears GRUB's saved entry without appending a `deadend`, `boot.saved_entry` or `shutdown` event, then exits without rebooting.

TOML configuration has no version field. Renamed or removed options are handled by the NixOS module and the changelog.

## Considered Options

- **Pin every session to its starting ruleset:** rejected because it requires shipping and maintaining old strategies inside new builds.
- **Migrate journals:** rejected because migrations could change the evidence used by an unfinished session.
- **Version the configuration:** rejected because effective configurable values are already recorded at every start.

## Consequences

A pre-1.0 change may require archiving a session and starting again. Build stamps identify the version that can continue it; older unstamped journals remain ruleset 1 with an unknown version. A schema mismatch is recognized from raw stamps before decoding later payloads, so even an unreadable journal can be archived without writing an event in the old schema. This exception must remain narrow: compatible runs continue to narrate their actions in the journal.
