import { Component, type ReactNode } from "react";

/**
 * Without this, one exception while rendering unmounts the whole React tree and the window goes
 * blank with no clue why. Show the error instead; the parent keys it per view so switching views resets it.
 */
export class ErrorBoundary extends Component<{ children: ReactNode }, { error: Error | null }> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: { componentStack?: string | null }) {
    console.error("UI render error:", error, info.componentStack);
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className="page">
        <div className="card">
          <h2>This screen failed to load</h2>
          <p className="error-text">{this.state.error.message}</p>
          <p className="muted small">Pick another view in the sidebar, or restart the app. If it keeps happening, please report the message above.</p>
        </div>
      </div>
    );
  }
}
