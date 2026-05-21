import { Copy } from "lucide-react";
import { lazy, memo, Suspense, useEffect, useRef, useState } from "react";
import { isDesktopApp, renderMemoPreview, renderMemoPreviewAsset } from "../tauri";
import type { PreviewRenderPath, RenderFormat, RenderPageAssetOutput, RenderTemplate } from "../types";

const MarkdownView = lazy(() => import("../MarkdownView"));
const PREVIEW_CACHE_MAX_ENTRIES = 18;
const PREVIEW_CACHE_MAX_BYTES = 12 * 1024 * 1024;
const PREVIEW_CACHE_VERSION = "typst-page-svg-v12";

type RenderState =
  | { kind: "idle" | "loading" | "markdown" }
  | {
      kind: "ready";
      svg?: string;
      url?: string;
      elapsedMs: number;
      cached: boolean;
      transport: "asset" | "inline";
      bytes?: number;
      widthPt?: number;
      heightPt?: number;
      pages?: PreviewPage[];
    }
  | { kind: "fallback"; message: string };

type PreviewPage = RenderPageAssetOutput & { svg?: string };

type CachedPreview = {
  url: string;
  elapsedMs: number;
  bytes: number;
  widthPt: number;
  heightPt: number;
  pages: PreviewPage[];
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

function TypstPreviewView({
  body,
  format,
  renderPath,
  template,
}: {
  body: string;
  format: RenderFormat;
  renderPath: PreviewRenderPath;
  template: RenderTemplate;
}) {
  const [state, setState] = useState<RenderState>({ kind: "idle" });
  const [copyText, setCopyText] = useState("Copy SVG");
  const requestIdRef = useRef(0);
  const effectivePath = renderPath === "auto" ? "typst-inline" : renderPath;

  useEffect(() => {
    const requestId = requestIdRef.current + 1;
    requestIdRef.current = requestId;
    if (!isDesktopApp || effectivePath === "markdown") {
      setState({ kind: "markdown" });
      return;
    }
    const key = `${PREVIEW_CACHE_VERSION}\0${effectivePath}\0${template}\0${previewCacheKey(body, format)}`;
    const cachedPreview = getCachedPreview(key);
    if (effectivePath === "typst-asset" && cachedPreview) {
      setState({
        kind: "ready",
        url: cachedPreview.url,
        elapsedMs: cachedPreview.elapsedMs,
        cached: true,
        transport: "asset",
        bytes: cachedPreview.bytes,
        widthPt: cachedPreview.widthPt,
        heightPt: cachedPreview.heightPt,
        pages: cachedPreview.pages,
      });
      return;
    }

    let cancelled = false;
    const handle = window.setTimeout(() => {
      setState((current) => (current.kind === "ready" ? current : { kind: "loading" }));
      void (async () => {
        if (effectivePath === "typst-asset") {
          try {
            const asset = await renderMemoPreviewAsset(body, format, template);
            const pages = await fetchPreviewPages(asset.pages.length ? asset.pages : [{ index: 0, url: asset.url, width_pt: asset.width_pt, height_pt: asset.height_pt, bytes: asset.bytes }]);
            if (!cancelled && requestIdRef.current === requestId) {
              putCachedPreview(key, {
                url: asset.url,
                elapsedMs: asset.elapsed_ms,
                bytes: asset.bytes,
                widthPt: asset.width_pt,
                heightPt: asset.height_pt,
                pages,
              });
              setState({
                kind: "ready",
                url: asset.url,
                elapsedMs: asset.elapsed_ms,
                cached: asset.cached,
                transport: "asset",
                bytes: asset.bytes,
                widthPt: asset.width_pt,
                heightPt: asset.height_pt,
                pages,
              });
            }
            return;
          } catch {
            // Fall back to the inline SVG path if the asset protocol cannot be fetched.
          }
        }

        try {
          const output = await renderMemoPreview(body, format, template);
          if (!cancelled && requestIdRef.current === requestId) {
            setState({
              kind: "ready",
              svg: output.svg,
              elapsedMs: output.elapsed_ms,
              cached: output.cached,
              transport: "inline",
              bytes: output.svg.length,
              widthPt: output.width_pt,
              heightPt: output.height_pt,
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
  }, [body, effectivePath, format, template]);

  async function copyRenderedSvg() {
    if (state.kind !== "ready") return;
    const svgText = state.svg ?? state.pages?.map((page) => page.svg).filter((svg): svg is string => Boolean(svg)).join("\n") ?? "";
    if (!svgText) return;
    try {
      await navigator.clipboard.writeText(svgText);
      setCopyText("Copied");
      window.setTimeout(() => setCopyText("Copy SVG"), 1200);
    } catch {
      setCopyText("Copy failed");
      window.setTimeout(() => setCopyText("Copy SVG"), 1400);
    }
  }

  if (state.kind === "ready") {
    const canCopySvg = Boolean(state.svg || state.pages?.some((page) => page.svg));
    return (
      <div className="typst-preview">
        <div className="render-status">
          <span>{state.transport === "asset" ? "Typst asset" : "Typst SVG"}</span>
          <span className="render-status-actions">
            <span>{state.cached ? "cache hit" : `${state.elapsedMs}ms`}</span>
            <button title={copyText} aria-label={copyText} onClick={copyRenderedSvg} disabled={!canCopySvg}>
              <Copy size={12} />
            </button>
          </span>
        </div>
        {state.url ? (
          <PreviewAsset
            heightPt={state.heightPt}
            onExpired={() => setState({ kind: "fallback", message: "Preview asset expired" })}
            pages={state.pages}
            url={state.url}
            widthPt={state.widthPt}
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

async function fetchPreviewPages(pages: RenderPageAssetOutput[]): Promise<PreviewPage[]> {
  return Promise.all(
    pages.map(async (page) => ({
      ...page,
      svg: await fetch(page.url, { cache: "force-cache" }).then((response) => {
        if (!response.ok) throw new Error(`Preview page ${page.index + 1} failed: ${response.status}`);
        return response.text();
      }),
    })),
  );
}

function PreviewAsset({
  heightPt,
  onExpired,
  pages,
  url,
  widthPt,
}: {
  heightPt?: number;
  onExpired: () => void;
  pages?: PreviewPage[];
  url: string;
  widthPt?: number;
}) {
  const visiblePages = pages?.length ? pages : [{ index: 0, url, width_pt: widthPt ?? 480, height_pt: heightPt ?? 640, bytes: 0 }];
  return (
    <div className="typst-preview-pages">
      {visiblePages.map((page) =>
        "svg" in page && typeof page.svg === "string" ? (
          <div
            key={`${page.index}:${page.url}`}
            className="typst-preview-asset"
            dangerouslySetInnerHTML={{ __html: page.svg }}
            style={{ aspectRatio: `${Math.max(page.width_pt, 1)} / ${Math.max(page.height_pt, 1)}` }}
          />
        ) : (
          <img
            key={`${page.index}:${page.url}`}
            alt={`Typst preview page ${page.index + 1}`}
            className="typst-preview-asset"
            decoding="async"
            loading={page.index === 0 ? "eager" : "lazy"}
            onError={onExpired}
            src={page.url}
            style={{ aspectRatio: `${Math.max(page.width_pt, 1)} / ${Math.max(page.height_pt, 1)}` }}
          />
        ),
      )}
    </div>
  );
}

export const TypstPreview = memo(TypstPreviewView);
