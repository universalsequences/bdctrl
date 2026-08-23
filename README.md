# beadsgpu

A Metal-native macOS viewer for [beads](https://github.com/steveyegge/beads) issue graphs.

This repository also contains an experimental Bun/OpenTUI ready-work viewer and Herdr agent launcher in [`tui/`](tui/README.md).

## Run

Requirements: macOS 14+, Xcode/Swift 6, and `bd` on `PATH`.

```sh
swift run beadsgpu /path/to/project
```

With no path, beadsgpu reopens the last project or offers a directory picker.

## Controls

- Drag empty canvas / two-finger scroll: pan
- Pinch: zoom to cursor
- Drag a node or epic shell: reposition
- Click a node or epic: details pane
- Double-click an epic: fit epic
- `⌘F`: find issue
- `⌘R`: refresh
- `⌘0`: fit graph
- `Esc`: deselect

Issue data and mutations go through `bd`; no JSONL files are read directly. Layout positions are saved under `~/Library/Application Support/beadsgpu/`.

## Development

```sh
swift build
swift test
```
