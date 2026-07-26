import { invoke } from "@tauri-apps/api/core";

// Minimal WARP-style settings/status window. Opened from the menu-bar icon,
// closes back into the menu bar (see the CloseRequested handler in lib.rs).
// Intentionally sparse for v1 — real controls (custom hotkey, language,
// translate toggle) land here later.
export default function Preferences() {
  return (
    <div className="min-h-screen w-full bg-neutral-900 text-neutral-100 select-none flex flex-col">
      <div className="flex-1 px-7 py-6 flex flex-col gap-5">
        <header className="flex items-center gap-3">
          <div className="h-9 w-9 rounded-xl bg-[#0a84ff]/20 flex items-center justify-center text-[#0a84ff] text-lg">
            ◍
          </div>
          <div>
            <h1 className="text-[15px] font-medium leading-tight">Rigg Voice</h1>
            <p className="text-[12px] text-neutral-400">Running in the menu bar</p>
          </div>
        </header>

        <section className="rounded-xl bg-neutral-800/60 divide-y divide-white/5">
          <Row label="Open dictation" value="Double-tap Fn (Globe)" />
          <Row label="Fallback shortcut" value="⌃ ⇧ R" />
          <Row label="Language" value="Auto-detect" />
        </section>

        <p className="text-[11.5px] leading-relaxed text-neutral-500">
          Rigg Voice keeps running in the menu bar after you close this window.
          Transcription happens entirely on your Mac.
        </p>

        <div className="mt-auto flex items-center justify-between">
          <span className="text-[11px] text-neutral-600">Version 0.1.0</span>
          <button
            onClick={() => invoke("quit_app")}
            className="rounded-lg bg-white/5 hover:bg-white/10 px-3 py-1.5 text-[12px] text-neutral-200 transition-colors"
          >
            Quit Rigg Voice
          </button>
        </div>
      </div>
    </div>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center justify-between px-4 py-2.5">
      <span className="text-[12.5px] text-neutral-300">{label}</span>
      <span className="text-[12.5px] text-neutral-400 tabular-nums">{value}</span>
    </div>
  );
}
