use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use whisper_rs::{WhisperContext, WhisperContextParameters, FullParams, SamplingStrategy};
use arboard::Clipboard;
use tauri::{Emitter, Manager};

#[cfg(target_os = "macos")]
mod fn_tap;
#[cfg(target_os = "macos")]
mod paste;

// ---- App state ----
struct Recording {
    flag: Arc<AtomicBool>,
    buffer: Arc<Mutex<Vec<f32>>>,
    sample_rate: Arc<AtomicU32>,
    handle: JoinHandle<()>,
}

#[derive(Default)]
struct AppState {
    rec: Mutex<Option<Recording>>,
    // Fast, small model for live streaming preview (base, ~142MB).
    whisper_streaming: Mutex<Option<Arc<WhisperContext>>>,
    // Quality model for the final transcription pass (medium, ~1.5GB).
    whisper_final: Mutex<Option<Arc<WhisperContext>>>,
    // The app that was frontmost when the bar was triggered — where we paste back.
    target_pid: Mutex<Option<i32>>,
}

// ---- Commands ----

// Start capturing from the mic. Two threads: one records audio, one keeps
// re-transcribing the growing buffer and streams the live text to the UI via
// the "partial" event — so words appear as you speak instead of the bar
// freezing until the end. `stop_recording` then does one clean final pass.
#[tauri::command]
fn start_recording(app: tauri::AppHandle, state: tauri::State<AppState>) -> Result<(), String> {
    let mut guard = state.rec.lock().unwrap();
    if guard.is_some() {
        return Err("already recording".into());
    }

    let ctx = get_streaming_context(&app, state.inner())?; // fast base model for live preview

    let flag = Arc::new(AtomicBool::new(true));
    let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
    let sample_rate = Arc::new(AtomicU32::new(16000));

    // 1) microphone capture
    let (f1, b1, s1) = (flag.clone(), buffer.clone(), sample_rate.clone());
    let handle = std::thread::spawn(move || recording_thread(f1, b1, s1));

    // 2) live streaming transcription — sliding window for constant latency.
    //    Instead of re-transcribing the entire recording every cycle, we only
    //    send the last ~8 seconds to Whisper. Earlier text is "confirmed" and
    //    kept as a prefix. This keeps each inference pass fast regardless of
    //    how long the recording runs.
    let (f2, b2, s2) = (flag.clone(), buffer.clone(), sample_rate.clone());
    let ctx2 = ctx.clone();
    let app2 = app.clone();
    std::thread::spawn(move || {
        let mut last_len = 0usize;
        let mut confirmed = String::new(); // text from earlier windows
        let window_secs: f32 = 8.0;        // sliding window size
        let overlap_secs: f32 = 1.5;       // overlap to avoid cutting words

        while f2.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(450));
            if !f2.load(Ordering::Relaxed) {
                break;
            }
            let (samples, sr) = {
                let b = b2.lock().unwrap();
                (b.clone(), s2.load(Ordering::Relaxed))
            };
            if samples.len() <= last_len {
                continue; // nothing new captured since last pass
            }
            last_len = samples.len();

            let window_samples = (window_secs * sr as f32) as usize;
            let overlap_samples = (overlap_secs * sr as f32) as usize;

            // If the buffer is longer than the window, confirm earlier text
            // and only transcribe the tail.
            let slice = if samples.len() > window_samples {
                // The portion before (buffer - window + overlap) is "done".
                // Transcribe it once to lock in the confirmed prefix.
                let confirm_end = samples.len() - window_samples + overlap_samples;
                if confirmed.is_empty() && confirm_end > 0 {
                    let early = resample_to_16k(&samples[..confirm_end], sr);
                    if let Ok(t) = run_whisper(&ctx2, &early) {
                        confirmed = t;
                    }
                }
                // Only send the last `window_samples` to Whisper.
                &samples[samples.len() - window_samples..]
            } else {
                &samples[..]
            };

            let mono = resample_to_16k(slice, sr);
            if mono.len() < 8000 {
                continue; // under ~0.5s — too little to transcribe well yet
            }
            if let Ok(tail) = run_whisper(&ctx2, &mono) {
                let full = if confirmed.is_empty() {
                    tail
                } else {
                    format!("{} {}", confirmed.trim(), tail.trim())
                };
                let _ = app2.emit("partial", full);
            }
        }
    });

    *guard = Some(Recording { flag, buffer, sample_rate, handle });
    Ok(())
}

// Stop recording, do one final (best-quality) transcription, copy to clipboard.
//
// This is an `async` command and the CPU-heavy Whisper pass runs inside
// `spawn_blocking`, so it NEVER touches the UI/main thread — that's what was
// causing the macOS spinning-wait cursor. We grab the audio + model handle
// cheaply up front (no lock is held across an await), then offload the work.
#[tauri::command]
async fn stop_recording(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    // Cheap, synchronous hand-off: stop the mic, snapshot samples, clone the
    // (Arc) model. All guards are dropped before we await anything.
    let (samples, sr, ctx, use_final) = {
        let recording = state.rec.lock().unwrap().take().ok_or("not recording")?;
        recording.flag.store(false, Ordering::Relaxed);
        let _ = recording.handle.join(); // mic thread exits within ~50ms
        let samples = recording.buffer.lock().unwrap().clone();
        let sr = recording.sample_rate.load(Ordering::Relaxed);
        // Try the quality model; fall back to the streaming model if it's not
        // ready yet (e.g. still downloading).
        let (ctx, use_final) = match get_final_context(&app, state.inner()) {
            Ok(c) => (c, true),
            Err(_) => (get_streaming_context(&app, state.inner())?, false),
        };
        (samples, sr, ctx, use_final)
    };

    let handle = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let mono = resample_to_16k(&samples, sr);
        if mono.is_empty() {
            return Err("no audio captured".into());
        }
        if use_final {
            run_whisper_final(&ctx, &mono)
        } else {
            run_whisper(&ctx, &mono)
        }
    });
    let text = match handle.await {
        Ok(res) => res?,
        Err(_) => return Err("transcription task failed".into()),
    };

    copy_to_clipboard(&text)?; // land the words on the clipboard
    Ok(text)
}

// Reactivate the app that was focused when the bar opened and paste the
// transcript (already on the clipboard) into it. This is the auto-paste half of
// the "Both" behaviour; the Copy button covers the clipboard half.
#[tauri::command]
fn paste_last(state: tauri::State<AppState>) {
    #[cfg(target_os = "macos")]
    {
        let pid = *state.target_pid.lock().unwrap();
        if let Some(pid) = pid {
            paste::paste_into(pid);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = state;
    }
}

// Dismiss the floating bar (Esc from the UI).
#[tauri::command]
fn hide_bar(window: tauri::Window) {
    let _ = window.hide();
}

// Abort the current recording and throw the audio away — no transcription, no
// clipboard, no paste. Backs both the Cancel and Restart controls.
#[tauri::command]
fn cancel_recording(state: tauri::State<AppState>) {
    if let Some(recording) = state.rec.lock().unwrap().take() {
        recording.flag.store(false, Ordering::Relaxed);
        let _ = recording.handle.join();
    }
}

// Transcribe an existing WAV file (kept for testing / future use).
#[tauri::command]
fn transcribe(
    app: tauri::AppHandle,
    path: String,
    state: tauri::State<AppState>,
) -> Result<String, String> {
    let mut reader = hound::WavReader::open(&path).map_err(|e| e.to_string())?;
    let spec = reader.spec();

    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max).map_err(|e| e.to_string()))
                .collect::<Result<_, _>>()?
        }
    };

    let channels = spec.channels as usize;
    let mono: Vec<f32> = if channels > 1 {
        raw.chunks(channels)
            .map(|f| f.iter().sum::<f32>() / channels as f32)
            .collect()
    } else {
        raw
    };
    let mono = resample_to_16k(&mono, spec.sample_rate);

    let ctx = get_final_context(&app, state.inner())?;
    let text = run_whisper_final(&ctx, &mono)?;
    copy_to_clipboard(&text)?;
    Ok(text)
}

// ---- Model management ----
//
// Models are resolved in two ways:
//  * In development (`tauri dev`, a debug build) we use the repo's `models/`
//    directory if it's there, so local iteration never re-downloads anything.
//  * In a packaged, shipped `.app` there is no such directory, so we download
//    the model on first run into the OS app-data dir and cache it there.
struct ModelSpec {
    filename: &'static str,
    url: &'static str,
}

// Fast model (base, ~142MB) — live streaming preview.
const STREAMING_MODEL: ModelSpec = ModelSpec {
    filename: "ggml-base.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
};
// Quality model (medium, ~1.5GB) — final transcription pass.
const FINAL_MODEL: ModelSpec = ModelSpec {
    filename: "ggml-medium.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.bin",
};

// Where a given model file should live on disk.
fn model_path(app: &tauri::AppHandle, filename: &str) -> Result<PathBuf, String> {
    // Dev builds: prefer the checked-out models/ dir for fast iteration.
    if cfg!(debug_assertions) {
        let dev = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../models")
            .join(filename);
        if dev.exists() {
            return Ok(dev);
        }
    }
    // Shipped app: cache under the OS app-data directory.
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("models");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join(filename))
}

// Ensure the model exists locally, downloading it on first run if needed.
fn ensure_model(app: &tauri::AppHandle, spec: &ModelSpec) -> Result<PathBuf, String> {
    let path = model_path(app, spec.filename)?;
    if !path.exists() {
        download_model(app, spec, &path)?;
    }
    Ok(path)
}

// Stream the model to disk, emitting "model-download" progress events the UI can
// show. Writes to a `.part` file first, then renames — so an interrupted
// download never leaves a half-file that looks complete.
fn download_model(app: &tauri::AppHandle, spec: &ModelSpec, dest: &Path) -> Result<(), String> {
    let resp = ureq::get(spec.url)
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    let total: u64 = resp
        .header("Content-Length")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let tmp = dest.with_extension("part");
    let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut reader = resp.into_reader();
    let mut buf = [0u8; 65536];
    let mut downloaded: u64 = 0;
    let mut last_emit = std::time::Instant::now();

    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        downloaded += n as u64;
        if last_emit.elapsed().as_millis() >= 200 {
            let percent = if total > 0 {
                (downloaded as f64 / total as f64 * 100.0) as u32
            } else {
                0
            };
            let _ = app.emit(
                "model-download",
                serde_json::json!({
                    "model": spec.filename,
                    "downloaded": downloaded,
                    "total": total,
                    "percent": percent,
                    "done": false
                }),
            );
            last_emit = std::time::Instant::now();
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())?;

    let _ = app.emit(
        "model-download",
        serde_json::json!({
            "model": spec.filename,
            "downloaded": downloaded,
            "total": total,
            "percent": 100u32,
            "done": true
        }),
    );
    Ok(())
}

// ---- Shared helpers ----

// Fast model (base, ~142MB) — loaded once, used for live streaming preview.
fn get_streaming_context(
    app: &tauri::AppHandle,
    state: &AppState,
) -> Result<Arc<WhisperContext>, String> {
    let mut guard = state.whisper_streaming.lock().unwrap();
    if guard.is_none() {
        let path = ensure_model(app, &STREAMING_MODEL)?;
        let path = path.to_str().ok_or("invalid model path")?;
        let ctx = WhisperContext::new_with_params(path, WhisperContextParameters::default())
            .map_err(|e| e.to_string())?;
        *guard = Some(Arc::new(ctx));
    }
    Ok(guard.as_ref().unwrap().clone())
}

// Quality model (medium, ~1.5GB) — loaded once, used for the final transcription.
fn get_final_context(
    app: &tauri::AppHandle,
    state: &AppState,
) -> Result<Arc<WhisperContext>, String> {
    let mut guard = state.whisper_final.lock().unwrap();
    if guard.is_none() {
        let path = ensure_model(app, &FINAL_MODEL)?;
        let path = path.to_str().ok_or("invalid model path")?;
        let ctx = WhisperContext::new_with_params(path, WhisperContextParameters::default())
            .map_err(|e| e.to_string())?;
        *guard = Some(Arc::new(ctx));
    }
    Ok(guard.as_ref().unwrap().clone())
}

// Run the streaming model — fast, rough preview. Transcription only.
fn run_whisper(ctx: &WhisperContext, samples: &[f32]) -> Result<String, String> {
    let mut state = ctx.create_state().map_err(|e| e.to_string())?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("auto"));
    params.set_translate(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state.full(params, samples).map_err(|e| e.to_string())?;

    let mut text = String::new();
    for segment in state.as_iter() {
        text.push_str(&segment.to_str_lossy().map_err(|e| e.to_string())?);
    }
    Ok(text.trim().to_string())
}

// Run the final model — high quality, auto-detected language.
fn run_whisper_final(ctx: &WhisperContext, samples: &[f32]) -> Result<String, String> {
    let mut state = ctx.create_state().map_err(|e| e.to_string())?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("auto"));
    params.set_translate(false); // set to true to enable translation to English
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state.full(params, samples).map_err(|e| e.to_string())?;

    let mut text = String::new();
    for segment in state.as_iter() {
        text.push_str(&segment.to_str_lossy().map_err(|e| e.to_string())?);
    }
    Ok(text.trim().to_string())
}

// Fatia 4: put text on the system clipboard so it can be pasted anywhere with Cmd+V.
fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;
    clipboard.set_text(text.to_string()).map_err(|e| e.to_string())
}

// Owns the cpal input stream for one recording. Pushes mono f32 into `buffer`
// until `flag` is cleared, then drops the stream (which stops capture).
fn recording_thread(flag: Arc<AtomicBool>, buffer: Arc<Mutex<Vec<f32>>>, sample_rate_out: Arc<AtomicU32>) {
    let host = cpal::default_host();
    let device = match host.default_input_device() {
        Some(d) => d,
        None => { eprintln!("no input device"); return; }
    };
    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => { eprintln!("input config error: {e}"); return; }
    };
    sample_rate_out.store(config.sample_rate(), Ordering::Relaxed);
    let channels = config.channels() as usize;
    let buf = buffer.clone();
    let err_fn = |e| eprintln!("audio stream error: {e}");

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config.into(),
            move |data: &[f32], _| {
                let mut b = buf.lock().unwrap();
                for frame in data.chunks(channels) {
                    b.push(frame.iter().sum::<f32>() / channels as f32);
                }
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            config.into(),
            move |data: &[i16], _| {
                let mut b = buf.lock().unwrap();
                for frame in data.chunks(channels) {
                    let sum: f32 = frame.iter().map(|s| *s as f32 / 32768.0).sum();
                    b.push(sum / channels as f32);
                }
            },
            err_fn,
            None,
        ),
        _ => { eprintln!("unsupported sample format"); return; }
    };

    let stream = match stream {
        Ok(s) => s,
        Err(e) => { eprintln!("build stream error: {e}"); return; }
    };
    if let Err(e) = stream.play() {
        eprintln!("stream play error: {e}");
        return;
    }

    while flag.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // `stream` is dropped here, which stops the capture.
}

// The mic runs at 44.1/48kHz; Whisper needs 16kHz. Simple linear resample.
fn resample_to_16k(input: &[f32], from_rate: u32) -> Vec<f32> {
    if from_rate == 16000 {
        return input.to_vec();
    }
    let ratio = 16000.0 / from_rate as f32;
    let out_len = (input.len() as f32 * ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f32 / ratio;
        let idx = src as usize;
        let frac = src - idx as f32;
        let a = input.get(idx).copied().unwrap_or(0.0);
        let b = input.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

// ---- Trigger + window placement ----

// Park the bar near the bottom-center of the screen it's on.
fn position_bottom_center<R: tauri::Runtime>(win: &tauri::WebviewWindow<R>) {
    let monitor = win
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| win.primary_monitor().ok().flatten());
    if let Some(monitor) = monitor {
        let screen = monitor.size();
        if let Ok(winsize) = win.outer_size() {
            let x = ((screen.width as i32) - (winsize.width as i32)) / 2;
            let margin = (monitor.scale_factor() * 96.0) as i32; // ~96pt above the Dock
            let y = (screen.height as i32) - (winsize.height as i32) - margin;
            let _ = win.set_position(tauri::PhysicalPosition::new(x.max(0), y.max(0)));
        }
    }
}

// Fired by the Fn double-tap or the Ctrl+Shift+R fallback. Remembers the app
// the user was in, then shows the bar and lets the UI toggle record/stop.
fn fire_trigger<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    #[cfg(target_os = "macos")]
    {
        if let Some(pid) = paste::frontmost_pid() {
            let me = std::process::id() as i32;
            if pid != me {
                if let Some(state) = app.try_state::<AppState>() {
                    *state.target_pid.lock().unwrap() = Some(pid);
                }
            }
        }
    }

    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = app2.get_webview_window("main") {
            position_bottom_center(&win);
            let _ = win.show();
            // Focus so Esc / keyboard reach the bar. We paste back to the stored
            // target PID regardless, so stealing focus here is harmless.
            let _ = win.set_focus();
        }
    });

    // Emit the toggle a beat AFTER showing, so the webview's "trigger" listener
    // is definitely registered — otherwise the very first event can be lost and
    // the bar shows but never starts recording.
    let app3 = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(90));
        let _ = app3.emit("trigger", ());
    });
}

// Show the Preferences window (declared hidden in tauri.conf.json). Opened from
// the tray "Preferences…" item.
fn show_preferences<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(win) = app.get_webview_window("preferences") {
        let _ = win.center();
        let _ = win.show();
        let _ = win.set_focus();
    }
}

// Quit the whole agent, called from the Preferences window's button. Carries a
// Some(code), so the ExitRequested guard in run() lets it through (window-close
// exits carry None and are blocked).
#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

// ---- App entry ----

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // Menu-bar-only agent. LSUIElement in Info.plist hides the Dock
            // icon in the packaged .app, but NOT under `tauri dev`, so we also
            // set the activation policy to Accessory at runtime. This drops the
            // Dock icon and the app menu bar in both dev and release, leaving
            // only the tray icon. (The old tauri#5122 window.show() bug that
            // discouraged this is fixed in current Tauri; if the floating bar
            // ever stops appearing on trigger, revisit here.)
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Menu-bar (tray) icon — the app's only visible home. Because the
            // Dock icon is hidden (LSUIElement), without this there is no way to
            // quit the agent short of Activity Monitor, and nowhere to hang
            // future settings. Menu: Start dictation + Quit.
            {
                use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
                use tauri::tray::TrayIconBuilder;

                let start_i =
                    MenuItem::with_id(app, "start", "Start dictation", true, None::<&str>)?;
                let prefs_i =
                    MenuItem::with_id(app, "prefs", "Preferences…", true, None::<&str>)?;
                let quit_i =
                    MenuItem::with_id(app, "quit", "Quit Rigg Voice", true, Some("Cmd+Q"))?;
                let sep = PredefinedMenuItem::separator(app)?;
                let menu = Menu::with_items(app, &[&start_i, &prefs_i, &sep, &quit_i])?;

                let mut tray = TrayIconBuilder::new()
                    .tooltip("Rigg Voice")
                    .menu(&menu)
                    .show_menu_on_left_click(true)
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "start" => fire_trigger(app),
                        "prefs" => show_preferences(app),
                        "quit" => app.exit(0),
                        _ => {}
                    });
                // Use the app icon if one is embedded; don't panic (unwrap) if
                // it isn't — an iconless tray is better than a crash.
                if let Some(icon) = app.default_window_icon().cloned() {
                    tray = tray.icon(icon);
                }
                let _tray = tray.build(app)?;
            }

            // WARP-style: closing the Preferences window hides it instead of
            // quitting the agent. The app keeps running in the menu bar.
            if let Some(prefs) = app.get_webview_window("preferences") {
                let prefs_hide = prefs.clone();
                prefs.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = prefs_hide.hide();
                    }
                });
            }

            if let Some(win) = app.get_webview_window("main") {
                // Native blurred glass. HudWindow is the most see-through
                // material — it shows the desktop *blurred* through dark glass
                // (light materials like Popover just frost to flat grey). Being
                // dark glass, it pairs with light text (see styles.css).
                #[cfg(target_os = "macos")]
                {
                    use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState};
                    let _ = apply_vibrancy(
                        &win,
                        NSVisualEffectMaterial::HudWindow,
                        Some(NSVisualEffectState::Active),
                        Some(20.0),
                    );
                }

                position_bottom_center(&win);
            }

            #[cfg(desktop)]
            {
                use tauri_plugin_global_shortcut::{
                    Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState,
                };

                // Reliable fallback trigger: Ctrl+Shift+R.
                let mods = Modifiers::CONTROL | Modifiers::SHIFT;
                let key = Code::KeyR;

                app.handle().plugin(
                    tauri_plugin_global_shortcut::Builder::new()
                        .with_handler(move |app, shortcut, event| {
                            if event.state() == ShortcutState::Pressed
                                && shortcut.matches(mods, key)
                            {
                                fire_trigger(app);
                            }
                        })
                        .build(),
                )?;

                app.global_shortcut()
                    .register(Shortcut::new(Some(mods), key))?;
            }

            // Primary trigger: Fn/Globe double-tap (needs Input Monitoring).
            #[cfg(target_os = "macos")]
            {
                let handle = app.handle().clone();
                fn_tap::spawn(move || fire_trigger(&handle));
            }

            // Warm both Whisper models at launch so the first recording is snappy.
            // Streaming model (base, ~142MB) loads fast; final model (medium,
            // ~1.5GB) loads in parallel.
            {
                let h1 = app.handle().clone();
                std::thread::spawn(move || {
                    if let Some(state) = h1.try_state::<AppState>() {
                        let _ = get_streaming_context(&h1, state.inner());
                    }
                });
                let h2 = app.handle().clone();
                std::thread::spawn(move || {
                    if let Some(state) = h2.try_state::<AppState>() {
                        let _ = get_final_context(&h2, state.inner());
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_recording,
            stop_recording,
            transcribe,
            paste_last,
            hide_bar,
            cancel_recording,
            quit_app
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            // Keep the agent alive when all windows are closed/hidden. An
            // explicit Quit (app.exit) carries Some(code) and is allowed
            // through; only window-close-driven exits (code: None) are blocked.
            if let tauri::RunEvent::ExitRequested { code, api, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}
