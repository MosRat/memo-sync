import { lazy, memo, Suspense, useEffect, useState } from "react";
import { isDesktopApp, renderMemoPreview } from "../tauri";
import type { RenderFormat } from "../types";

const MarkdownView = lazy(() => import("../MarkdownView"));

type RenderState =
  | { kind: "idle" | "loading" }
  | { kind: "ready"; svg: string; elapsedMs: number }
  | { kind: "fallback"; message: string };

function TypstPreviewView({ body, format }: { body: string; format: RenderFormat }) {
  const [state, setState] = useState<RenderState>({ kind: "idle" });

  useEffect(() => {
    if (!isDesktopApp) {
      setState({ kind: "fallback", message: "Web preview uses React Markdown" });
      return;
    }
    let cancelled = false;
    const handle = window.setTimeout(() => {
      setState((current) => (current.kind === "ready" ? current : { kind: "loading" }));
      renderMemoPreview(body, format)
        .then((output) => {
          if (!cancelled) setState({ kind: "ready", svg: output.svg, elapsedMs: output.elapsed_ms });
        })
        .catch((error) => {
          if (!cancelled) setState({ kind: "fallback", message: error instanceof Error ? error.message : String(error) });
        });
    }, 180);
    return () => {
      cancelled = true;
      window.clearTimeout(handle);
    };
  }, [body, format]);

  if (state.kind === "ready") {
    return (
      <div className="typst-preview">
        <div className="render-badge">Typst SVG · {state.elapsedMs}ms</div>
        <div dangerouslySetInnerHTML={{ __html: state.svg }} />
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
