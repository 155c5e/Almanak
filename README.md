# Almanak

**Photo, Vector and Layout in one window, on one document.** An Affinity-style creative suite
built from three open-source, pure-Rust apps by the [ArtCraft](https://getartcraft.com/) team:

| Persona | Built on | What it does |
|---|---|---|
| **Photo** | [PhotoCraft](https://github.com/storytold/photocraft) | Image editing: layers, masks, adjustments, real PSD files |
| **Vector** | [VectorCraft](https://github.com/storytold/vectorcraft) | Vector illustration |
| **Layout** | [DesignCraft](https://github.com/storytold/designcraft) | Page layout and publishing |

> **Status: early.** The three apps run as personas of one window today. Editing the same
> document across personas is the next milestone (see [Roadmap](#roadmap)).

## Run

```sh
cd almanak
cargo run --release                      # the persona bar is at the top of the window
cargo run --release -- poster.psd logo.svg mag.designcraft   # each file opens in its persona
```

Needs Rust 1.95+. The first build compiles all three apps and takes a while.

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

PhotoCraft and VectorCraft ship MCP servers (`photocraft-cli mcp`, `vectorcraft-cli mcp`) that
expose every command to agents; DesignCraft has one too. See each app's `docs/mcp.md`.

## Roadmap

1. ✅ Shell window with Photo / Vector / Layout personas.
2. One `.craft` document: a layout whose placed images and artwork are embedded PhotoCraft and
   VectorCraft documents, edited in place by switching persona (like Affinity's StudioLink).
3. Shared swatches, fonts and undo across personas; the apps' full native services
   (clipboard images, recovery, presets) in the shell.
4. Blender objects: `.blend` files placed as 3D objects, rendered with headless Blender
   (`blender -b`), with Edit in Blender. Blender runs as a separate program only (it's GPL).
5. Maybe: one layer model shared by all personas.

## License

MIT OR Apache-2.0, like the apps it's built on. PhotoCraft, VectorCraft and DesignCraft are by
their contributors (see each folder's `LICENSE-*`, `NOTICE` and contributors list). The ArtCraft
logos in their `docs/brand/` folders are trademarks under their own license and are not part of
Almanak's branding. Almanak is an independent project, not affiliated with ArtCraft or Serif/Affinity.
