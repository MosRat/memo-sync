# Typst Rendering Research

## Decision

Typst is a realistic replacement for our hand-styled Markdown preview, but it should be introduced as a native rendering service rather than as a direct React component. The recommended first production path is:

1. Keep editing as plain text in the app.
2. Store a memo render mode: `markdown` or `typst`.
3. For Markdown, wrap the body in a small Typst document that imports `@preview/cmarker:0.1.8` and calls `cmarker.render(...)`.
4. Compile in Rust through the Typst crate.
5. Export preview pages as SVG and display them in the WebView.
6. Use PDF export for share/export later.

HTML export exists, but Typst currently treats it as experimental, so SVG preview is the safer app path.

## Rust API Surface

Typst is published as Rust crates. The main `typst` crate exposes the compiler and documents that compilation flows through parsing, evaluation, layout, and export to PDF, PNG, SVG, or HTML. The crate exposes `typst::compile`.

The app must provide a `World` implementation. `World` supplies:

- standard library
- font book
- main file id
- source loading
- binary file loading
- font lookup
- current date

The docs explicitly recommend caching in `World`, especially fonts and repeated source/file access. That is important for our desktop preview loop.

Useful crates:

- `typst = "0.14.2"`: compiler and core types
- `typst-svg = "0.14.2"`: export frame/page or merged document SVG
- `typst-pdf = "0.14.2"`: PDF export
- `typst-assets = "0.14.2"`: bundled default fonts/assets if needed

## Markdown Through cmarker

`cmarker` is a Typst Universe package that transpiles CommonMark Markdown to Typst content from inside Typst. Basic usage:

```typst
#import "@preview/cmarker:0.1.8"
#cmarker.render(read("memo.md"))
```

For our app, avoid reading arbitrary paths in the first version. Generate a virtual main Typst source:

```typst
#import "@preview/cmarker:0.1.8"
#set page(width: auto, height: auto, margin: 0pt)
#set text(font: ("Noto Serif CJK SC", "New Computer Modern"), size: 12pt)

#cmarker.render(read("memo.md"), raw-typst: false)
```

Then provide `memo.md` from an in-memory virtual file through `World::source`/`World::file`.

## Proposed App Architecture

Added a new crate:

```text
crates/memo-render/
```

Responsibilities:

- `RenderInput { body, language, theme, width, fonts }`
- `RenderOutput { pages: Vec<SvgPage>, diagnostics, elapsed_ms }`
- `MemoWorld`: in-memory Typst world with cached fonts and virtual files
- markdown wrapper generation
- typst direct compilation
- SVG export

First implementation status:

- `memo-render` exists and can compile direct Typst input to merged SVG.
- Tauri exposes `render_memo_preview`.
- Desktop preview calls the Rust renderer first and falls back to React Markdown when Typst/cmarker fails.
- Markdown rendering is wired through the cmarker Typst package path, so the first successful render may need package download/cache access.
- `memo.md` is provided through Typst's binary file resolver because cmarker uses `read(...)`, not an import/source load.
- A direct Typst smoke test runs in the normal Rust suite. A cmarker Markdown smoke test is present but ignored by default because it may download the package on first use.

Tauri commands:

```rust
render_memo_preview(input: RenderMemoInput) -> RenderMemoOutput
export_memo_pdf(input: RenderMemoInput) -> Vec<u8>
```

Frontend:

- keep textarea/editor unchanged
- replace current React Markdown preview with `TypstPreview`
- debounce render calls separately from save calls
- show diagnostics inline beside the preview
- cache by `memo_id + body_hash + render_mode + width`

## Performance Notes

- Run rendering off the UI thread with `tauri::async_runtime::spawn_blocking`.
- Reuse font book and loaded fonts across renders.
- Keep a small LRU cache of compiled SVG output by body hash.
- Limit preview to changed memo, not the whole list.
- Prefer SVG strings for preview; they are easy to embed and avoid raster scaling blur.
- For very large notes, render first page quickly and stream additional pages later.

## Security Notes

- Disable cmarker `raw-typst` for Markdown by default.
- Use a virtual file system rooted to the memo attachment directory.
- Do not allow Typst `read()` to escape the memo workspace.
- Decide explicitly whether remote image URLs are allowed.
- Limit body size and render timeout to protect the app from expensive documents.

## Integration Steps

1. Add `memo-render` crate with a minimal `World` and a Typst smoke test.
2. Add a Tauri `render_memo_preview` command returning merged SVG.
3. Add `render_mode` to memo metadata or settings: `markdown`, `typst`.
4. Switch preview pane to call Rust renderer behind a feature flag.
5. Add cmarker package resolution cache.
6. Add diagnostics UI and fallback to current Markdown preview.
7. Add PDF export command.

## Sources

- Typst docs: https://typst.app/docs/
- Typst repository: https://github.com/typst/typst
- `typst` crate: https://docs.rs/typst/latest/typst/
- `World` trait: https://docs.rs/typst/latest/typst/trait.World.html
- `typst-svg`: https://docs.rs/typst-svg/latest/typst_svg/
- `typst-pdf`: https://docs.rs/typst-pdf/latest/typst_pdf/
- cmarker package: https://typst.app/universe/package/cmarker/
