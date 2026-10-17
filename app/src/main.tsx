import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { GamepadProvider } from "./gamepad/GamepadProvider";
import { StoreProvider } from "./store";
import "./styles.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <StoreProvider>
      <GamepadProvider>
        <App />
      </GamepadProvider>
    </StoreProvider>
  </React.StrictMode>,
);
