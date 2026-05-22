import { memo, useEffect, useMemo, useRef, useState } from "react";
import type { Memo } from "../types";
import { memoPreviewText } from "../search";

const overscan = 4;

function smoothScrollBehavior(): ScrollBehavior {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth";
}

function useNarrowMemoList() {
  const [narrow, setNarrow] = useState(() => (typeof window === "undefined" ? false : window.matchMedia("(max-width: 640px)").matches));

  useEffect(() => {
    const query = window.matchMedia("(max-width: 640px)");
    const update = () => setNarrow(query.matches);
    update();
    query.addEventListener("change", update);
    return () => query.removeEventListener("change", update);
  }, []);

  return narrow;
}

function MemoListView({
  memos,
  activeMemoId,
  selectedIds,
  density,
  onSelect,
  onToggleSelected,
}: {
  memos: Memo[];
  activeMemoId: string | null;
  selectedIds: Set<string>;
  density: "comfortable" | "compact";
  onSelect: (id: string) => void;
  onToggleSelected: (id: string) => void;
}) {
  const scrollerRef = useRef<HTMLDivElement>(null);
  const [viewport, setViewport] = useState({ top: 0, height: 0 });
  const narrowList = useNarrowMemoList();
  const rowHeight = narrowList ? (density === "compact" ? 84 : 112) : density === "compact" ? 96 : 128;
  const activeIndex = memos.findIndex((item) => item.id === activeMemoId);
  const range = useMemo(() => {
    const start = Math.max(0, Math.floor(viewport.top / rowHeight) - overscan);
    const count = Math.ceil((viewport.height || 520) / rowHeight) + overscan * 2;
    const end = Math.min(memos.length, start + count);
    return { start, end };
  }, [memos.length, viewport.height, viewport.top]);
  const rendered = memos.slice(range.start, range.end);

  useEffect(() => {
    const element = scrollerRef.current;
    if (!element) return;
    const update = () => setViewport({ top: element.scrollTop, height: element.clientHeight });
    update();
    element.addEventListener("scroll", update, { passive: true });
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(update);
    observer?.observe(element);
    const fallback = observer ? null : window.setInterval(update, 500);
    return () => {
      element.removeEventListener("scroll", update);
      observer?.disconnect();
      if (fallback !== null) window.clearInterval(fallback);
    };
  }, []);

  useEffect(() => {
    const element = scrollerRef.current;
    if (!element || activeIndex < 0) return;
    const top = activeIndex * rowHeight;
    const bottom = top + rowHeight;
    const behavior = smoothScrollBehavior();
    if (top < element.scrollTop) {
      element.scrollTo({ top, behavior });
    } else if (bottom > element.scrollTop + element.clientHeight) {
      element.scrollTo({ top: bottom - element.clientHeight, behavior });
    }
  }, [activeIndex]);

  if (!memos.length) {
    return (
      <div className="memo-list empty">
        <div className="memo-list-empty">
          <strong>No matching memos</strong>
          <span>Try a different search, tag, or repository.</span>
        </div>
      </div>
    );
  }

  return (
    <div className="memo-list" ref={scrollerRef}>
      <div className="memo-list-spacer" style={{ height: memos.length * rowHeight }}>
        {rendered.map((item, index) => {
          const absoluteIndex = range.start + index;
          return (
            <button
              key={item.id}
              className={`${activeMemoId === item.id ? "memo-row active" : "memo-row"} ${selectedIds.has(item.id) ? "selected" : ""} ${density === "compact" ? "compact" : ""}`}
              style={{ transform: `translateY(${absoluteIndex * rowHeight}px)` }}
              onClick={() => onSelect(item.id)}
            >
              <span
                role="checkbox"
                aria-checked={selectedIds.has(item.id)}
                tabIndex={0}
                className="memo-row-check"
                onClick={(event) => {
                  event.stopPropagation();
                  onToggleSelected(item.id);
                }}
                onKeyDown={(event) => {
                  if (event.key !== " " && event.key !== "Enter") return;
                  event.preventDefault();
                  event.stopPropagation();
                  onToggleSelected(item.id);
                }}
              />
              <div>
                <strong>{item.title}</strong>
                <p>{memoPreviewText(item.body_md) || "Empty memo"}</p>
              </div>
              <footer>
                <span>{new Date(item.updated_at).toLocaleDateString()}</span>
                {item.pinned && <em>Pinned</em>}
                {item.archived && <em>Archived</em>}
                {item.source !== "Manual" && <em>{item.source === "QuickCapture" ? "Quick" : item.source}</em>}
                {item.tags.slice(0, 2).map((tag) => (
                  <em key={tag}>#{tag}</em>
                ))}
              </footer>
            </button>
          );
        })}
      </div>
    </div>
  );
}

export const MemoList = memo(MemoListView);
