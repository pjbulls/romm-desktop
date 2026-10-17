import { useStore } from "../store";

export function Toasts() {
  const { toasts, dismiss } = useStore();
  return (
    <div className="toasts" role="region" aria-label="Notifications" aria-live="polite">
      {toasts.map((t) => (
        <div key={t.id} className={`toast toast-${t.tone}`} role={t.tone === "error" ? "alert" : "status"}>
          <div className="toast-body">
            <strong>{t.text}</strong>
            {t.detail?.map((d, i) => <div key={i} className="toast-line">{d}</div>)}
          </div>
          <button className="icon-btn" onClick={() => dismiss(t.id)} aria-label="Dismiss notification">×</button>
        </div>
      ))}
    </div>
  );
}
