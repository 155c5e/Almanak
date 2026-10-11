# Almanak

**Photo, Vector and Layout in one window, on one document.** An Affinity-style creative suite
built from three open-source, pure-Rust apps by the [ArtCraft](https://getartcraft.com/) team:

| Persona | Built on | What it does |
|---|---|---|
| **Photo** | [PhotoCraft](https://github.com/storytold/photocraft) | Image editing: layers, masks, adjustments, real PSD files |
| **Vector** | [VectorCraft](https://github.com/storytold/vectorcraft) | Vector illustration |
| **Layout** | [DesignCraft](https://github.com/storytold/designcraft) | Page layout and publishing |

> **Status: early.** Photo, Vector and Layout personas in one window, with graphics placed in a
> layout edited in place in Photo and Vector (StudioLink).

## Run

```sh
cd almanak
cargo run --release                      # the persona bar is at the top of the window
cargo run --release -- poster.psd logo.svg mag.designcraft   # each file opens in its persona
```

Needs Rust 1.95+. The first build compiles all three apps and takes a while.

## One document, three personas

The shared document is the layout's `.designcraft` file: it embeds every placed graphic.

1. In **Layout**, select a placed graphic.
2. Switch to **Photo** (raster images) or **Vector** (PDF, SVG): the graphic opens there.
3. Edit it, then switch back to **Layout**: the edit is in the layout, as one undo step
   (Edit › Undo "Edit Graphic").

Photo writes the graphic back as PSD (layers kept); Vector as PDF with Preserve Editing (the full
VectorCraft document travels inside, so it reopens editable). Only real edits are written: looking
or selecting in Photo or Vector leaves the graphic as it was. Switching to the persona again with
the same graphic selected returns to its open document. A graphic opens by what its bytes are,
never by the name the layout gives it.

Edits go into the layout document in front; one whose layout is behind another waits until that
layout comes to the front. Quitting writes pending edits into the layout first, then asks about
each persona's unsaved documents in turn. Settings are kept in `~/.config/almanak/`.

## How it's put together

- `photocraft/`, `vectorcraft/`, `designcraft/`: the upstream apps, vendored with `git subtree`
  (squashed). Almanak's changes to them are kept to small hooks (for example
  `reinstall_style()`) so upstream work keeps merging.
- `almanak/`: the shell. It owns one window and one egui context, hosts the three apps' UI
  objects, and switches between them from the persona bar.

Pull in upstream changes:

```sh
git subtree pull --prefix=photocraft  https://github.com/storytold/photocraft  main --squash
git subtree pull --prefix=vectorcraft https://github.com/storytold/vectorcraft main --squash
git subtree pull --prefix=designcraft https://github.com/storytold/designcraft main --squash
```

## Agents

`--control <port>` starts a JSON-lines control channel per persona, each speaking that app's own
protocol, all behind the same bearer token (the first line must be
`{"id":"auth","method":"auth","params":{"token":"…"}}`):

| Persona | Port | Protocol |
|---|---|---|
| Photo | `port` | [photocraft/docs/control-protocol.md](photocraft/docs/control-protocol.md) |
| Vector | `port + 1` | [vectorcraft/docs/control-protocol.md](vectorcraft/docs/control-protocol.md) |
| Layout | `port + 2` | [designcraft/docs/control-protocol.md](designcraft/docs/control-protocol.md) |

A request on a persona's port brings that persona to the front. `almanak.switch
{"persona": "photo" | "vector" | "layout"}`, on any port, does what the persona bar does
(StudioLink included) and replies with its status line.

```sh
cargo run --release -- --control 7979 --control-token-file ~/.config/almanak/photo.token \
  --automation-read-root ~/work --automation-write-root ~/work
```

The automation roots confine the files agents may read and write: Photo's (none given: no file
access) and Vector's (none given: unconfined). **Layout's channel is not confined**: anyone with
the token can read and write your files through it. Without `--control-token-file` a token is
generated and printed. Photo's MCP bridge works as with PhotoCraft:
`photocraft-cli mcp --bridge 127.0.0.1:7979 --control-token-file …`. VectorCraft's and
DesignCraft's own clients don't send a token, so they need the auth line added.
PhotoCraft and VectorCraft also ship headless MCP servers (`photocraft-cli mcp`,
`vectorcraft-cli mcp`); see each app's `docs/mcp.md`.

## Roadmap

1. ✅ Shell window with Photo / Vector / Layout personas.
2. ✅ StudioLink: placed graphics edited in place in Photo and Vector. Next: Save in Photo/Vector
   writes into the layout instead of asking for a file, live updates without switching, writing
   back off the UI thread (large PSDs stall the window today), graphics in table cells.
3. Shared swatches, fonts and undo across personas; the apps' full native services
   (clipboard images, recovery, presets) in the shell.
4. Blender objects: `.blend` files placed as 3D objects, rendered with headless Blender
   (`blender -b`), with Edit in Blender. Blender runs as a separate program only (it's GPL).
5. Integrated terminal panel, for running Claude Code (or any agent) beside the document, wired to
   the control channels.
6. Maybe: one layer model shared by all personas.

## License

MIT OR Apache-2.0, like the apps it's built on. PhotoCraft, VectorCraft and DesignCraft are by
their contributors (see each folder's `LICENSE-*`, `NOTICE` and contributors list). The ArtCraft
logos in their `docs/brand/` folders are trademarks under their own license and are not part of
Almanak's branding. Almanak is an independent project, not affiliated with ArtCraft or Serif/Affinity.
