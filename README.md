# Rigg Voice

Press a hotkey, speak, and your words land as text in whatever app is in front of you — transcribed (and optionally translated to English) on-device by Whisper Large V3 Turbo. No cloud, no API keys, no per-minute cost.

Rigg Voice lives in the macOS menu bar with a small configuration window. Everything runs locally.

## Status

Early development. Built in vertical slices:

- [x] Whisper Turbo proven on-device (whisper.cpp)
- [ ] Tauri app scaffold
- [ ] Transcribe a WAV via `whisper-rs`
- [ ] Record from the microphone
- [ ] Copy transcript to the clipboard / paste into the active app
- [ ] Global hotkey + menu-bar icon
- [ ] Packaging + release

## Tech stack

- **Tauri** (Rust backend + TypeScript frontend)
- **whisper-rs** — Rust bindings for whisper.cpp, running the Large V3 Turbo model locally
- Menu-bar app with a minimal configuration window

## Requirements

- macOS (Apple Silicon recommended)
- Microphone permission and Accessibility permission (for the global hotkey and pasting)

## Develop

```bash
npm install
npm run tauri dev
```

## Build

```bash
npm run tauri build
```

## Model

The Whisper model (~1.5 GB) is **not** committed to the repo. It is downloaded on first run and cached locally.

## License

MIT — see [LICENSE](./LICENSE).
