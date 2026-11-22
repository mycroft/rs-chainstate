---
name: safety
description: Never writing to bitcoind's own chainstate
applies_to: ["src/**/*.rs", "README.md"]
---
Opening a LevelDB database writes to it, so rs-chainstate must never open
bitcoind's live `chainstate/` directory. `snapshot` copies the files without
opening them and adds a marker; every other command refuses a directory
without the marker (unless `--force`) or whose `LOCK` is held through an
fcntl lock. Review the change for:

- A command that opens a database before both checks, or skips them on some
  path (a new subcommand, an error branch, a default argument).
- `snapshot` opening the source as a database, writing into the source, or
  producing a copy that is inconsistent without saying so.
- The lock probe taking the lock instead of only querying it (`F_GETLK`),
  or treating a probe error as "not locked".
- `unsafe` code whose invariants the change breaks.

Report only concrete, reachable problems in the changed code.
