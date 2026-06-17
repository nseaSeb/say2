# say2

A terminal app for drilling everyday English sentences. Browse a library of
phrases, hear them read aloud, and run a hands-free "play" mode that speaks
random sentences on a loop so you can practise listening and repeating.

> ⚠️ **Content warning.** The bundled `sentences.toml` is a personal learning
> deck and may contain slang, informal, or vulgar expressions used as language
> examples. Edit or replace it to suit your own taste.

## macOS only

**say2 works on macOS only.** Speech is produced by shelling out to the
built-in [`say`](https://ss64.com/mac/say.html) command, which ships with
macOS. There is no fallback for Linux or Windows — on those platforms the UI
runs but nothing is spoken (the `say` process simply fails to launch).

The Settings screen exposes the macOS-specific `say -v <voice>` and
`say -r <rate>` flags, so voices are the ones installed under
**System Settings → Accessibility → Spoken Content**.

## Requirements

- macOS (for the `say` command)
- [Rust](https://www.rust-lang.org/tools/install) (stable, 2024 edition)

## Build & run

```sh
cargo run --release
```

On first launch say2 creates a config file at
`~/.config/say2/sentences.toml`, seeded with a starter set of sentences. Edit
it directly, or add and edit sentences from inside the app.

## Keys

| Key            | Action                                   |
| -------------- | ---------------------------------------- |
| `j` / `k`, ↑/↓ | move selection                           |
| `p` / `Enter`  | speak the selected sentence              |
| `space`        | play / stop auto mode                    |
| `/`            | search (by text or tag)                  |
| `a`            | add a sentence                           |
| `e`            | edit the selected sentence               |
| `d`            | delete the selected sentence             |
| `m`            | star / unstar (starred plays more often) |
| `s`            | settings (voice / rate / star weight)    |
| `+` / `-`      | pause length between auto-played lines   |
| `?`            | help                                     |
| `q`            | quit                                     |
