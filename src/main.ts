import { invoke } from "@tauri-apps/api/core";

async function record() {
  const out = document.querySelector("#out")!;
  out.textContent = "Recording 5s…";
  try {
    out.textContent = await invoke<string>("record_and_transcribe", { seconds: 5 });
  } catch (e) {
    out.textContent = "Error: " + e;
  }
}

window.addEventListener("DOMContentLoaded", () => {
  document.querySelector("#rec")?.addEventListener("click", record);
});