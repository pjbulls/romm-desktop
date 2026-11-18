import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { startDiagnostics } from "./diagnostics";
import { GamepadProvider } from "./gamepad/GamepadProvider";
import { StoreProvider } from "./store";
import "./styles.css";

startDiagnostics();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary scope="app">
      <StoreProvider>
        <GamepadProvider>
          <App />
        </GamepadProvider>
      </StoreProvider>
    </ErrorBoundary>
  </React.StrictMode>,
);
