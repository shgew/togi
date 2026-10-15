# Only shared-hardware tests are serialized

Status: see [the index](README.md).

Supersedes [ADR 0062](0062-checks-need-no-race-detector-or-test-result-cache.md)'s separate process-test concurrency cap; its other decisions remain.

Filesystem and process tests own their temporary paths and child processes. They use nextest's default global concurrency, without a separate group limit or an explicit restatement of that default. The `real_fs_` and `real_process_` prefixes classify kernel-dependent tests; a nextest group is a semaphore, not a label, and a two-test cap cannot exclude races over a shared resource. Longer process-test timeouts remain.

Hardware tests still run one at a time: SMU tests change and restore offsets on the same CPU, and scope tests share systemd and the host lock. Their serialization is a correctness boundary, not a performance throttle. VM hardware invocations and `just hardware` keep their single-threaded execution.

A new concurrency cap needs a concrete shared-resource constraint or measured resource-exhaustion evidence, not the fact that a test touches the filesystem or starts a process.
