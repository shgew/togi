# Clean-room rewrite, not a port of linux-corecycler

The owner's fork of linux-corecycler works but became too large to understand: a Qt GUI, a monitoring subsystem, a SQLite history, seven validation stages, annealing and multi-boot crash hunts. togi takes facts from it (SMU command IDs, backend quirks, failure modes) and none of its structure. `docs/prior-art.md` lists what was taken and what was rejected, so rejected ideas are not reintroduced by accident.
