import { memo } from "react";

export type ToastKind = "info" | "success" | "warning" | "error";

export interface ToastMessage {
  id: number;
  kind: ToastKind;
  title: string;
  detail?: string;
}

function ToastStackView({ toasts, onDismiss }: { toasts: ToastMessage[]; onDismiss: (id: number) => void }) {
  if (!toasts.length) return null;
  return (
    <div className="toast-stack" role="status" aria-live="polite">
      {toasts.map((toast) => (
        <button key={toast.id} className={`toast ${toast.kind}`} onClick={() => onDismiss(toast.id)}>
          <strong>{toast.title}</strong>
          {toast.detail && <span>{toast.detail}</span>}
        </button>
      ))}
    </div>
  );
}

export const ToastStack = memo(ToastStackView);
