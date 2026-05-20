import { lazy, memo, Suspense, useEffect, useRef, useState } from "react";
import { isDesktopApp, renderMemoPreview, renderMemoPreviewAsset } from "../tauri";
import type { RenderFormat } from "../types";

const MarkdownView = lazy(() => import("../MarkdownView"));
const PREVIEW_CACHE_MAX_ENTRIES = 18;
const PREVIEW_CACHE_MAX_BYTES = 12 * 1024 * 1024;

type RenderState =
  | { kind: "idle" | "loading" }
  | {
      kind: "ready";
      svg?: string;
      url?: string;
      elapsedMs: number;
      cached: boolean;
      transport: "asset" | "ipc";
      bytes?: number;
    }
  | { kind: "fallback"; message: string };

type CachedPreview = {
  url: string;
  elapsedMs: number;
  bytes: number;
};

const previewCache = new Map<string, CachedPreview>();
let previewCacheBytes = 0;

function previewCacheKey(body: string, format: RenderFormat) {
  return `${format}\0${body}`;
}

function getCachedPreview(key: string) {
  const cached = previewCache.get(key);
  if (!cached) {
    return null;
  }
  previewCache.delete(key);
  previewCache.set(key, cached);
  return cached;
}

function putCachedPreview(key: string, preview: CachedPreview) {
  const bytes = preview.bytes;
  if (bytes > PREVIEW_CACHE_MAX_BYTES) {
    return;
  }

  const existing = previewCache.get(key);
  if (existing) {
    previewCacheBytes -= existing.bytes;
    previewCache.delete(key);
  }

  previewCache.set(key, preview);
  previewCacheBytes += bytes;

  while (previewCache.size > PREVIEW_CACHE_MAX_ENTRIES || previewCacheBytes > PREVIEW_CACHE_MAX_BYTES) {
    const oldestKey = previewCache.keys().next().value;
    if (!oldestKey) {
      break;
    }
    const oldest = previewCache.get(oldestKey);
    previewCache.delete(oldestKey);
    previewCacheBytes -= oldest?.bytes ?? 0;
  }
}

function debounceForBody(body: string) {
  if (body.length > 80000) {
    return 850;
  }
  if (body.length > 30000) {
    return 520;
  }
  if (body.length > 9000) {
    return 300;
  }
  return 160;
}

function TypstPreviewView({ body, format }: { body: string; format: RenderFormat }) {
  const [state, setState] = useState<RenderState>({ kind: "idle" });
  const requestIdRef = useRef(0);

  useEffect(() => {
    const requestId = requestIdRef.current + 1;
    requestIdRef.current = requestId;
    if (!isDesktopApp) {
      setState({ kind: "fallback", message: "Web preview uses React Markdown" });
      return;
    }
    const key = previewCacheKey(body, format);
    const cachedPreview = getCachedPreview(key);
    if (cachedPreview) {
      setState({
        kind: "ready",
        url: cachedPreview.url,
        elapsedMs: cachedPreview.elapsedMs,
        cached: true,
        transport: "asset",
        bytes: cachedPreview.bytes,
      });
      return;
    }

    let cancelled = false;
    const handle = window.setTimeout(() => {
      setState((current) => (current.kind === "ready" ? current : { kind: "loading" }));
      void (async () => {
        try {
          const asset = await renderMemoPreviewAsset(body, format);
          if (!cancelled && requestIdRef.current === requestId) {
            putCachedPreview(key, { url: asset.url, elapsedMs: asset.elapsed_ms, bytes: asset.bytes });
            setState({
              kind: "ready",
              url: asset.url,
              elapsedMs: asset.elapsed_ms,
              cached: asset.cached,
              transport: "asset",
              bytes: asset.bytes,
            });
          }
          return;
        } catch {
          // Fall back to the legacy IPC SVG path on platforms where the custom protocol is unavailable.
        }

        try {
          const output = await renderMemoPreview(body, format);
          if (!cancelled && requestIdRef.current === requestId) {
            setState({
              kind: "ready",
              svg: output.svg,
              elapsedMs: output.elapsed_ms,
              cached: output.cached,
              transport: "ipc",
              bytes: output.svg.length,
            });
          }
        } catch (error) {
          if (!cancelled && requestIdRef.current === requestId) {
            setState({ kind: "fallback", message: error instanceof Error ? error.message : String(error) });
          }
        }
      })();
    }, debounceForBody(body));
    return () => {
      cancelled = true;
      window.clearTimeout(handle);
    };
  }, [body, format]);

  if (state.kind === "ready") {
    return (
      <div className="typst-preview">
        <div className="render-status">
          <span>{state.transport === "asset" ? "Typst asset" : "Typst SVG"}</span>
          <span>{state.cached ? "cache hit" : `${state.elapsedMs}ms`}</span>
        </div>
        {state.url ? (
          <object
            aria-label="Typst preview"
            className="typst-preview-asset"
            data={state.url}
            onError={() => setState({ kind: "fallback", message: "Preview asset expired" })}
            type="image/svg+xml"
          />
        ) : (
          <div dangerouslySetInnerHTML={{ __html: state.svg ?? "" }} />
        )}
      </div>
    );
  }

  return (
    <>
      {state.kind === "loading" && <p className="markdown-loading">Rendering with Typst...</p>}
      {state.kind === "fallback" && <p className="render-warning">Typst fallback: {state.message}</p>}
      <Suspense fallback={<p className="markdown-loading">Rendering preview...</p>}>
        <MarkdownView>{body}</MarkdownView>
      </Suspense>
    </>
  );
}

export const TypstPreview = memo(TypstPreviewView);
