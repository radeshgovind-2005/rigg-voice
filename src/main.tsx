import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import Preferences from "./Preferences";
import "./styles.css";

// Both windows load the same bundle; render by window label. The floating
// dictation bar is "main"; the settings window is "preferences".
const isPreferences = getCurrentWindow().label === "preferences";

createRoot(document.getElementById("root")!).render(
  <StrictMode>{isPreferences ? <Preferences /> : <App />}</StrictMode>
);
