# Contributing

## Never attach your export

`export.xml` is medical data: measurements, dates, device names, and — in
workout routes — the coordinates of your home and workplace.

A bug report needs the **structure**, not the content: a few elements with the
values replaced, plus the version line from the top of the file
(`<!-- HealthKit Export Version: NN -->`) and your iOS version.

## Fixtures are synthetic, without exception

Nobody's real data belongs in `tests/fixtures/` — including data someone else
has already published. A new case goes in as an invented example reproducing
the structure.

This is not theoretical: the overlapping-sleep bug was found on a live export,
and what was committed is `sleep-overlap.xml`, written from scratch to
reproduce the same shape.

## Tests

Each test runs the built binary and asserts on what reached the database, not
on a function in isolation. A fix that cannot be shown as a failing test before
and a green one after is probably fixing the wrong thing.

```
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Out of scope

Importing routes and GPX, keeping raw samples, and dependencies added for
convenience. The reasons are in the README.
