import { memo } from "react";

export type ToastKind = "info" | "success" | "warning" | "error";

export interface ToastMessage {
  id: number;
  kind: ToastKind;
  title: string;
  detail?: string;
  actionLabel?: string;
  action?: () => void | Promise<void>;
}

function ToastStackView({ toasts, onDismiss }: { toasts: ToastMessage[]; onDismiss: (id: number) => void }) {
  if (!toasts.length) return null;
  return (
    <div className="toast-stack" role="status" aria-live="polite">
      {toasts.map((toast) => (
        <div key={toast.id} className={`toast ${toast.kind}`}>
          <button className="toast-body" onClick={() => onDismiss(toast.id)}>
            <strong>{toast.title}</strong>
            {toast.detail && <span>{toast.detail}</span>}
          </button>
          {toast.action && toast.actionLabel && (
            <button
              className="toast-action"
              onClick={() => {
                onDismiss(toast.id);
                void toast.action?.();
              }}
            >
              {toast.actionLabel}
            </button>
          )}
        </div>
      ))}
    </div>
  );
}

export const ToastStack = memo(ToastStackView);
