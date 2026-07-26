# Rigg Voice

Press a key, speak, and your words appear as text in whatever app is in front of you. Rigg Voice is a macOS menu-bar agent that does fast, fully on-device speech-to-text with Whisper — no cloud, no API keys, no per-minute cost. Everything runs locally on your Mac.

Double-tap the Fn (Globe) key anywhere, a small glass bar slides up from the bottom of the screen, you talk, and the transcript is pasted straight back into the field you were in.

---

## What makes it interesting (the engineering)

Most "speak into a box" tools wait until you stop talking and then transcribe everything at once, so the UI freezes for a few seconds at the end. Rigg Voice is built around a **dual-model streaming architecture** that shows words as you speak while still producing a high-quality final result.

### Dual-model streaming

Two Whisper models are loaded at launch and used for different jobs:

- **Streaming model — `base` (~142 MB).** Small and fast. It runs continuously while you talk to produce a live preview, so text appears in the bar in near real time.
- **Final model — `medium` (~1.5 GB).** Slower but far more accurate. It runs exactly once, the moment you stop, to produce the clean transcript that actually gets pasted.

You get the responsiveness of a tiny model during dictation and the accuracy of a large one in the result — instead of compromising on a single model for both.

### Sliding-window transcription (constant latency)

Naively, live preview means re-transcribing the entire recording every cycle — which gets slower and slower the longer you talk. Rigg Voice avoids that with a **sliding window**:

- Only the last ~8 seconds of audio are sent to the streaming model on each pass, with a ~1.5 s overlap so words aren't cut at the boundary.
- Once audio scrolls out of that window, it's transcribed one final time and kept as a "confirmed" text prefix.
- Each live pass is therefore roughly constant cost, no matter whether you've been talking for 5 seconds or 5 minutes.

The confirmed prefix plus the fresh window tail are concatenated and streamed to the UI via a `partial` event.

### Threading model (the UI never blocks)

Speech-to-text is CPU-heavy, so none of it is allowed near the main/UI thread:

- **Mic thread** — owns the `cpal` input stream and pushes mono `f32` samples into a shared buffer until recording stops.
- **Streaming thread** — wakes every ~450 ms, grabs the latest window, transcribes it with the `base` model, and emits the live `partial` text.
- **Final pass** — runs inside `spawn_blocking`, so the heavy `medium`-model inference happens off the async runtime entirely. (This is what eliminated the macOS spinning-wait cursor.)

Locks are only ever held briefly to snapshot data; no lock is held across an `.await`.

### Audio pipeline

The microphone runs at 44.1/48 kHz, but Whisper expects 16 kHz mono. Incoming frames are downmixed to mono on the capture thread (averaging channels), then linearly resampled to 16 kHz before inference. WAV files can also be transcribed directly (via `hound`) for testing.

### Global trigger — Fn/Globe double-tap

The Fn (Globe) key is invisible to the normal macOS hotkey API (Carbon `RegisterEventHotKey`) — it only shows up as a modifier-flag change. To catch a *double-tap* of Fn, Rigg Voice installs a **`CGEventTap`** on the session event stream, watches `flagsChanged` events, and times the gap between two Fn key-down edges. This is hand-written CoreGraphics FFI so it stays stable without version-sensitive wrapper crates. A `Ctrl+Shift+R` global shortcut is registered as a reliable fallback.

### Auto-paste to the right app

When the bar is triggered, Rigg Voice records the **PID of the frontmost app** (the window you were typing in). After transcription it delivers a synthetic `Cmd+V` *directly to that process* with `CGEventPostToPid` — so the text lands in the field you left, without stealing focus or activating windows. The clipboard is also populated, so manual paste keeps working too.

### Menu-bar-only agent

There's no Dock icon and no app menu. The macOS activation policy is set to `Accessory` at runtime (and `LSUIElement` in the packaged `Info.plist`), leaving only a menu-bar (tray) icon as the app's home. Closing the Preferences window hides it back into the menu bar rather than quitting; the agent keeps running.

### The floating glass bar

The dictation bar is a borderless, transparent, always-on-top window positioned bottom-center. It uses `window-vibrancy` with the `HudWindow` material to show the real desktop *blurred* through dark glass (the only way to blur the live desktop on macOS), with a React UI on top.

---

## Model delivery

Models are resolved in two ways:

- **Development** (`tauri dev`, a debug build) uses the repo's local `models/` directory if present, so iteration never re-downloads anything.
- **Shipped app** downloads the model on first run from Hugging Face into the OS app-data directory (`~/Library/Application Support/com.radeshgovind.rigg-voice/models/`) and caches it there. Downloads stream to a `.part` file and are renamed on completion, and progress is emitted to the UI via a `model-download` event.

Model files are never committed to the repo.

---

## Permissions

Rigg Voice needs three macOS permissions (System Settings → Privacy & Security):

- **Microphone** — to record your voice.
- **Input Monitoring** — for the Fn double-tap event tap.
- **Accessibility** — to post the synthetic paste keystroke into other apps.

Until Input Monitoring is granted, the event tap can't be created and the app falls back to the `Ctrl+Shift+R` shortcut.

---

## Tech stack

- **Tauri 2** — Rust backend, web frontend, native macOS window.
- **whisper-rs** (with Metal) — on-device Whisper inference.
- **cpal** — cross-platform microphone capture.
- **hound** — WAV reading.
- **arboard** — clipboard.
- **window-vibrancy** — native blurred-glass material.
- Raw CoreGraphics/CoreFoundation FFI — Fn double-tap event tap and targeted auto-paste.
- **React** — the dictation bar and Preferences UI.

---

## Install

### Homebrew (recommended)

```bash
brew tap radeshgovind-2005/rigg-voice
brew trust radeshgovind-2005/rigg-voice
brew install --cask rigg-voice
```

The `brew trust` step is required once — Homebrew asks you to explicitly trust any third-party tap before installing from it.

### Manual

Download the latest `.dmg` from the [Releases](../../releases) page and drag Rigg Voice to Applications.

### First launch

Because the app isn't code-signed yet, macOS Gatekeeper will warn you the first time. Open it once with **right-click → Open** (or System Settings → Privacy & Security → "Open Anyway"). After that it launches normally. The Whisper models download automatically on first run.

## Develop

Requires Rust, Node, and the Xcode Command Line Tools.

```bash
npm install
npm run tauri dev
```

For local development, drop the two model files into a `models/` directory at the repo root:

- `models/ggml-base.bin`
- `models/ggml-medium.bin`

(Get them from the [whisper.cpp Hugging Face repo](https://huggingface.co/ggerganov/whisper.cpp).) In a shipped build these download automatically on first run.

## Build

```bash
npm run tauri build
```

## License

MIT — see [LICENSE](./LICENSE).
