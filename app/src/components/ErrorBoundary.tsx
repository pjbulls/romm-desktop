import { Component, type ReactNode } from "react";

interface Props {
  children: ReactNode;
  /**
   * "view" (default): one screen. The parent keys it per view, so switching views resets it.
   * "app": around everything, the last line of defence. An exception in the sidebar, the toasts or
   * a provider is outside every view, and without this it left an empty dark window with no message.
   */
  scope?: "view" | "app";
}

interface S {
  error: Error | null;
  stack: string | null;
}

/**
 * Without this, one exception while rendering unmounts the whole React tree and the window goes
 * blank with no clue why. Show the error instead.
 */
export class ErrorBoundary extends Component<Props, S> {
  state: S = { error: null, stack: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: { componentStack?: string | null }) {
    console.error("UI render error:", error, info.componentStack);
    this.setState({ stack: info.componentStack ?? null });
  }

  render() {
    const { error, stack } = this.state;
    if (!error) return this.props.children;
    if (this.props.scope === "app") {
      return (
        <div className="center-screen" data-testid="app-error" role="alert">
          <div className="card" style={{ maxWidth: 720 }}>
            <h1>RomM Desktop hit an unexpected error</h1>
            <p className="error-text">{error.message}</p>
            <p className="muted small">Nothing was lost: your games, saves and settings are untouched. Reloading usually gets you back. If it keeps happening, please report the message above and the details below.</p>
            <div className="actions">
              <button className="btn btn-primary" onClick={() => window.location.reload()} data-testid="app-error-reload">Reload</button>
            </div>
            <details>
              <summary className="muted small">Details</summary>
              <pre className="small" style={{ whiteSpace: "pre-wrap", userSelect: "text", maxHeight: 220, overflow: "auto" }}>{`${error.stack ?? error.message}${stack ? `\n\nComponent stack:${stack}` : ""}`}</pre>
            </details>
          </div>
        </div>
      );
    }
    return (
      <div className="page">
        <div className="card">
          <h2>This screen failed to load</h2>
          <p className="error-text">{error.message}</p>
          <p className="muted small">Pick another view in the sidebar, or restart the app. If it keeps happening, please report the message above.</p>
        </div>
      </div>
    );
  }
}
