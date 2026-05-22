import { Copy } from "lucide-react";
import { lazy, memo, Suspense, useEffect, useRef, useState } from "react";
import { attachmentRefsFromMarkdown, bodyHasOnlyAttachmentImages } from "../attachments";
import { isMobileApp, isNativeApp, renderMemoPreview, renderMemoPreviewAsset } from "../tauri";
import type { PreviewRenderPath, RenderFormat, RenderPageAssetOutput, RenderTemplate } from "../types";

const MarkdownView = lazy(() => import("../MarkdownView"));
const PREVIEW_CACHE_MAX_ENTRIES = 48;
const PREVIEW_CACHE_MAX_BYTES = 512 * 1024;
const PREVIEW_CACHE_VERSION = "typst-page-svg-v18";

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
      pages?: RenderPageAssetOutput[];
    }
  | { kind: "fallback"; message: string };

type CachedPreview = {
  url: string;
  elapsedMs: number;
  bytes: number;
  widthPt: number;
  heightPt: number;
  pages: RenderPageAssetOutput[];
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
  const bytes = cachedPreviewSize(preview);
  if (bytes > PREVIEW_CACHE_MAX_BYTES) {
    return;
  }

  const existing = previewCache.get(key);
  if (existing) {
    previewCacheBytes -= cachedPreviewSize(existing);
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
    previewCacheBytes -= oldest ? cachedPreviewSize(oldest) : 0;
  }
}

function cachedPreviewSize(preview: CachedPreview) {
  return preview.url.length + preview.pages.reduce((total, page) => total + page.url.length + 40, 80);
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
  resolveImageUrl,
  template,
}: {
  body: string;
  format: RenderFormat;
  renderPath: PreviewRenderPath;
  resolveImageUrl?: (url: string) => string;
  template: RenderTemplate;
}) {
  const [state, setState] = useState<RenderState>({ kind: "idle" });
  const [copyText, setCopyText] = useState("Copy SVG");
  const requestIdRef = useRef(0);
  const effectivePath = renderPath === "auto" ? (isMobileApp ? "markdown" : "typst-asset") : renderPath;
  const pureImageAttachmentIds = bodyHasOnlyAttachmentImages(body) ? attachmentRefsFromMarkdown(body) : [];
  const shouldUseMarkdown = !isNativeApp || effectivePath === "markdown";

  useEffect(() => {
    const requestId = requestIdRef.current + 1;
    requestIdRef.current = requestId;
    if (pureImageAttachmentIds.length > 0 || shouldUseMarkdown) {
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
            const pages = asset.pages.length ? asset.pages : [{ index: 0, url: asset.url, width_pt: asset.width_pt, height_pt: asset.height_pt, bytes: asset.bytes }];
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
            if (cancelled) return;
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
  }, [body, effectivePath, format, pureImageAttachmentIds.length, shouldUseMarkdown, template]);

  async function copyRenderedSvg() {
    if (state.kind !== "ready") return;
    try {
      const svgText = state.svg ?? (await fetchSvgPages(state.pages ?? []));
      if (!svgText) return;
      await navigator.clipboard.writeText(svgText);
      setCopyText("Copied");
      window.setTimeout(() => setCopyText("Copy SVG"), 1200);
    } catch {
      setCopyText("Copy failed");
      window.setTimeout(() => setCopyText("Copy SVG"), 1400);
    }
  }

  if (state.kind === "ready") {
    const canCopySvg = Boolean(state.svg || state.pages?.length);
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

  if (state.kind === "fallback") {
    return (
      <>
        <details className="render-warning">
          <summary>Preview fell back to plain text</summary>
          <pre>{state.message}</pre>
        </details>
        <pre className="fallback-text">{body}</pre>
      </>
    );
  }

  if (pureImageAttachmentIds.length > 0) {
    return (
      <div className="image-preview-only">
        {pureImageAttachmentIds.map((id) => (
          <img key={id} alt="" decoding="async" loading="lazy" src={resolveImageUrl?.(`memo-attachment:${id}`) ?? ""} />
        ))}
      </div>
    );
  }

  return (
    <>
      {state.kind === "loading" && <p className="markdown-loading">Rendering with Typst...</p>}
      <Suspense fallback={<p className="markdown-loading">Rendering preview...</p>}>
        <MarkdownView resolveImageUrl={resolveImageUrl}>{body}</MarkdownView>
      </Suspense>
    </>
  );
}

async function fetchSvgPages(pages: RenderPageAssetOutput[]) {
  const svgs = await Promise.all(
    pages.map((page) =>
      fetch(page.url, { cache: "force-cache" }).then((response) => {
        if (!response.ok) throw new Error(`Preview page ${page.index + 1} failed: ${response.status}`);
        return response.text();
      }),
    ),
  );
  return svgs.join("\n");
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
  pages?: RenderPageAssetOutput[];
  url: string;
  widthPt?: number;
}) {
  const visiblePages = pages?.length ? pages : [{ index: 0, url, width_pt: widthPt ?? 480, height_pt: heightPt ?? 640, bytes: 0 }];
  return (
    <div className="typst-preview-pages">
      {visiblePages.map((page) => (
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
      ))}
    </div>
  );
}

export const TypstPreview = memo(TypstPreviewView);
