# JSON for every first-party data file

Status: Accepted.

Configuration, bench suites, simulator machines and fit outputs used TOML while journals, snapshots, extracts and evaluation records already used JSON or JSON Lines. The TOML files needed no feature JSON lacks except comments, leaving two parsers for the same kind of data ([#622](https://github.com/shgew/togi/issues/622)).

## Decision

Every first-party data file uses standard JSON: JSON objects for documents and JSON Lines for event or record streams. Configuration, suites, machines and fit outputs move to `.json`, and the TOML parser is removed. Formats owned by external tools, including mprime, y-cruncher and GRUB, are unchanged.

Meaningful comments become data. Hand-written machine explanations use a `description` string. Generated fits use a `notes` array of strings for generated and bootstrap provenance, constrained-refit explanations, flagged model checks and not-forward-validated qualifications. Metadata describes the machine without changing its simulated behavior.

Configuration keeps its snake_case keys, defaults and strict unknown-member checks, including nested objects. The default path is `/etc/togi/config.json`, and the NixOS module emits JSON from `services.togi.settings`. [Runtime Configuration](../spec/runtime.md#configuration) owns the configuration contract.

This is a clean cutover, not a compatibility mode: loaders do not accept TOML, JSON with comments or YAML. Committed machine and fit values convert without refitting. The journal's `config.loaded` snapshot keeps its existing shape, so the file-format change does not change the journal schema or require a reset.

## Considered Options

- **Keep TOML:** rejected. These files use objects, arrays and scalar values that JSON already represents; a second parser and format buy only comment syntax.
- **JSON with comments:** rejected. A nonstandard JSON dialect would still need another parser or a preprocessing step. Explanations and qualifications that matter belong in fields that readers and generators preserve.
- **YAML:** rejected. It adds another parser and a broader syntax and type system without a requirement these data files need.

## Consequences

Operators with a hand-written configuration convert its contents and update its path; NixOS module users rebuild. Examples and generated files use valid JSON with no comments. Existing journals and historical fixture snapshots remain untouched.
