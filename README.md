# bdctrl

Two complementary interfaces for [Beads](https://github.com/steveyegge/beads):

- [`tui/`](tui/README.md) — the OpenTUI command center and Herdr agent launcher
- [`gui/`](gui/README.md) — a GPUI bird's-eye dashboard for epics and priorities

Both use the `bd` CLI as their source of truth.

## TUI

```sh
cd tui
bun install
bun start -- /path/to/project
```

## GUI

```sh
cd gui
cargo run -- /path/to/project
```

The original Swift/Metal experiment remains under `Sources/` and `Tests/` for now, but is not the active direction of the project.
