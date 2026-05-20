# Realtime Preview Architecture

## What We Can Borrow

Tinymist and the older Typst Preview extension optimize for latency with a native Rust backend, web SVG preview, partial rendering, and a websocket data plane. The older preview project describes its model clearly: file changes are compiled incrementally to SVG, SVG changes are sent to the client, and the client applies a VDOM-style diff. Tinymist now consolidates this preview workflow and exposes `tinymist preview ... --partial-rendering`.

VS Code's built-in Markdown preview is a different design point. It uses a WebView preview, can dynamically follow the active document or be locked to a file, and lets extensions add CSS, markdown-it plugins, and scripts. Those extensions are activated lazily when a Markdown preview is first shown, and preview scripts are reloaded on content changes. This is cheap for Markdown because HTML rendering is lightweight compared with Typst layout.

Tauri's communication model matters because rendered SVG can become large. `invoke` is fine for request-response preview output, but Tauri's docs warn that event-style frontend calls are not suitable for large data and recommend Channels for ordered streaming workloads. Tauri also supports custom protocol URLs and the asset protocol, which is a better fit when large preview assets should be fetched by the WebView instead of serialized through JSON IPC on every update.

## Current Implementation

```text
React editor
  -> debounced render request
  -> Tauri invoke(render_memo_preview)
  -> Rust render cache lookup
  -> spawn_blocking Typst compile on miss
  -> Typst/cmarker -> merged SVG
  -> React injects SVG
```

Implemented safeguards:

- frontend request id: stale Typst results are ignored
- frontend debounce: avoids compiling on every keystroke, with larger delays for larger documents
- frontend LRU cache: repeated previews in the same app session skip IPC and Rust rendering
- Rust `spawn_blocking`: Typst compilation does not block the async runtime
- Rust LRU cache: 96 entries, 24 MiB max SVG bytes
- cache key: `format + body`
- cmarker test: ignored by default but manually runnable because package fetch may use network

## Next Architecture

### Stage 1: Current App Path

Keep `invoke` for complete SVG results while memos are small and medium sized.

Add:

- render time budget telemetry
- optional manual render for very large documents

Implemented now:

- frontend LRU cache, so toggling edit/preview avoids IPC entirely
- adaptive debounce by document size

### Stage 2: Asset-backed Preview

For larger SVGs, change `render_memo_preview` to return metadata:

```json
{
  "cacheKey": "...",
  "url": "memo-preview://svg/<cacheKey>",
  "elapsedMs": 42,
  "cached": false
}
```

Then the WebView fetches SVG through a custom protocol. This avoids repeatedly serializing huge SVG strings through JSON IPC and lets the WebView cache/fetch like a normal resource.

### Stage 3: Streaming / Partial Rendering

For long documents:

- render pages separately
- send page metadata first
- stream page SVGs over Tauri Channel, ordered by page index
- update only changed page DOM nodes
- keep previous pages visible while changed pages render

This is the closest analogue to Tinymist/typst-preview's partial rendering and client-side SVG diff strategy, but it fits Tauri without requiring a websocket server.

### Stage 4: Source/Preview Sync

Typst has IDE-oriented crates and preview systems use source-span information for jump-to-source. The first production version should add coarse sync:

- scroll editor to heading when preview heading is clicked
- preserve preview scroll ratio across rerenders
- later: use Typst source spans for exact click-to-source

## Sources

- Tinymist preview docs: https://myriad-dreamin.github.io/tinymist/feature/preview.html
- Typst Preview architecture summary: https://github.com/Enter-tainer/typst-preview
- VS Code Markdown docs: https://code.visualstudio.com/docs/languages/markdown
- VS Code Markdown extension API: https://vscode-docs1.readthedocs.io/en/latest/extensionAPI/api-markdown/
- Tauri calling frontend / Channels: https://v2.tauri.app/develop/calling-frontend/
- Tauri calling Rust / Channels: https://v2.tauri.app/develop/calling-rust/
- Tauri config custom protocol references: https://v2.tauri.app/reference/config/
