# Every workload checks its own results

Production studies at Google and Meta found cores that return wrong results without crashing or logging anything. A load generator that only produces heat cannot see those. Every togi workload therefore comes from a backend that verifies its own computation: mprime or y-cruncher. Load steps and medium duty cycles are produced by suspending and resuming those same processes, not by a separate load generator.

## Considered Options

- **stress-ng `--cpu-load` and `--cpu-load-slice`:** convenient duty cycles, but no result verification.
- **stressapptest:** targets the memory controller more than cores; deferred to [#19](https://github.com/shgew/togi/issues/19).
