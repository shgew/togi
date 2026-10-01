# The journal is the source of truth; no database

A session produces at most a few megabytes of events per day for one CPU. togi records everything in an append-only JSONL journal and derives the state from it by replay. `state.json` is only a projection for readers. This makes crash recovery a replay, makes the simulator and the real machine produce comparable traces, and keeps every decision readable with a text editor or by an agent.

## Considered Options

- **SQLite (as in linux-corecycler):** transactional, but opaque to plain reading and a second representation of the same history.
- **A mutable state file without history:** loses the causal trail the owner wants for tracing every action.
