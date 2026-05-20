import { memo, useEffect, useMemo, useRef, useState } from "react";
import type { Memo } from "../types";

const rowHeight = 124;
const overscan = 4;

function MemoListView({
  memos,
  activeMemoId,
  onSelect,
}: {
  memos: Memo[];
  activeMemoId: string | null;
  onSelect: (id: string) => void;
}) {
  const scrollerRef = useRef<HTMLDivElement>(null);
  const [viewport, setViewport] = useState({ top: 0, height: 0 });
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
    const observer = new ResizeObserver(update);
    observer.observe(element);
    return () => {
      element.removeEventListener("scroll", update);
      observer.disconnect();
    };
  }, []);

  useEffect(() => {
    const element = scrollerRef.current;
    if (!element || activeIndex < 0) return;
    const top = activeIndex * rowHeight;
    const bottom = top + rowHeight;
    if (top < element.scrollTop) {
      element.scrollTo({ top, behavior: "smooth" });
    } else if (bottom > element.scrollTop + element.clientHeight) {
      element.scrollTo({ top: bottom - element.clientHeight, behavior: "smooth" });
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
              className={activeMemoId === item.id ? "memo-row active" : "memo-row"}
              style={{ transform: `translateY(${absoluteIndex * rowHeight}px)` }}
              onClick={() => onSelect(item.id)}
            >
              <div>
                <strong>{item.title}</strong>
                <p>{item.body_md.replace(/[#*_`]/g, "").slice(0, 110) || "Empty memo"}</p>
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
