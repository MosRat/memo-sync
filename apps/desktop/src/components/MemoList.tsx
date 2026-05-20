import { memo } from "react";
import type { Memo } from "../types";

function MemoListView({
  memos,
  activeMemoId,
  onSelect,
}: {
  memos: Memo[];
  activeMemoId: string | null;
  onSelect: (id: string) => void;
}) {
  return (
    <div className="memo-list">
      {memos.map((item) => (
        <button
          key={item.id}
          className={activeMemoId === item.id ? "memo-row active" : "memo-row"}
          onClick={() => onSelect(item.id)}
        >
          <div>
            <strong>{item.title}</strong>
            <p>{item.body_md.replace(/[#*_`]/g, "").slice(0, 110) || "Empty memo"}</p>
          </div>
          <span>{new Date(item.updated_at).toLocaleDateString()}</span>
        </button>
      ))}
    </div>
  );
}

export const MemoList = memo(MemoListView);
