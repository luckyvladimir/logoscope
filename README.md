# logoscope

A fast, terminal-based log file viewer built with Rust and [ratatui](https://github.com/ratatui/ratatui).

## Features

- **File & stdin support** — open a log file directly or pipe output from any command
- **Vim-style navigation** — `j`/`k`, `Ctrl-d`/`Ctrl-u`, `g`/`G`, page up/down
- **Search** (`/`) — case-insensitive search with match highlighting, `n`/`N` to jump between matches
- **Filter** (`\`) — show only lines matching a query, stackable with search
- **Follow mode** (`F` or `G`) — automatically tail new output from stdin
- **Detail panel** (`Space`) — split view with JSON pretty-printing and syntax coloring
- **Open in editor** (`Enter`) — send the selected line to `$EDITOR`

## Installation

```sh
cargo install --path .
```

## Usage

```sh
# View a log file
logoscope app.log

# Pipe from another command
tail -f /var/log/system.log | logoscope
```

## Keybindings

| Key | Action |
|---|---|
| `j` / `k` | Move cursor down / up |
| `Ctrl-d` / `Ctrl-u` | Half-page down / up |
| `Ctrl-f` / `Ctrl-b` | Full page down / up |
| `g` / `G` | Go to top / bottom |
| `F` | Toggle follow mode |
| `/` | Search |
| `n` / `N` | Next / previous match |
| `\` | Filter lines |
| `Space` | Toggle detail panel |
| `Enter` | Open line in `$EDITOR` |
| `Esc` | Close panel / clear filter / clear search |
| `q` | Quit |

## License

MIT
