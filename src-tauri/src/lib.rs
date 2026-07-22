use std::sync::{Arc, Mutex};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use whisper_rs::{WhisperContext, WhisperContextParameters, FullParams, SamplingStrategy};

// ---- Commands (called from the frontend) ----

// Transcribe an existing WAV file on disk.
#[tauri::command]
fn transcribe(path: String) -> Result<String, String> {
    let mut reader = hound::WavReader::open(&path).map_err(|e| e.to_string())?;
    let spec = reader.spec();

    // Read every sample as f32, whatever the source format.
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

    // Downmix to mono, then resample to the 16kHz Whisper wants.
    let channels = spec.channels as usize;
    let mono: Vec<f32> = if channels > 1 {
        raw.chunks(channels)
            .map(|f| f.iter().sum::<f32>() / channels as f32)
            .collect()
    } else {
        raw
    };
    let mono = resample_to_16k(&mono, spec.sample_rate);

    transcribe_samples(mono)
}

// Record `seconds` from the mic, then transcribe.
#[tauri::command]
fn record_and_transcribe(seconds: u32) -> Result<String, String> {
    let samples = record_samples(seconds)?;
    transcribe_samples(samples)
}

// ---- Shared helpers ----

// The one place the Whisper model actually runs. Expects 16kHz mono f32.
fn transcribe_samples(samples: Vec<f32>) -> Result<String, String> {
    let model_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../models/ggml-large-v3-turbo.bin");
    let model_path = model_path.to_str().ok_or("invalid model path")?;

    let ctx = WhisperContext::new_with_params(model_path, WhisperContextParameters::default())
        .map_err(|e| e.to_string())?;
    let mut state = ctx.create_state().map_err(|e| e.to_string())?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("pt"));   // you speak Portuguese
    params.set_translate(true);        // ...output in English
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state.full(params, &samples).map_err(|e| e.to_string())?;

    let mut text = String::new();
    for segment in state.as_iter() {
        text.push_str(&segment.to_str_lossy().map_err(|e| e.to_string())?);
    }
    Ok(text.trim().to_string())
}

// Records `seconds` from the default input device and returns 16kHz mono f32.
fn record_samples(seconds: u32) -> Result<Vec<f32>, String> {
    let host = cpal::default_host();
    let device = host.default_input_device().ok_or("no input device")?;
    let config = device.default_input_config().map_err(|e| e.to_string())?;
    let sample_rate = config.sample_rate();
    let channels = config.channels() as usize;

    let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
    let buf = buffer.clone();
    let err_fn = |e| eprintln!("audio stream error: {e}");

    // Mic usually gives f32 on macOS; handle i16 too just in case.
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
        _ => return Err("unsupported sample format".into()),
    }
    .map_err(|e| e.to_string())?;

    stream.play().map_err(|e| e.to_string())?;
    std::thread::sleep(std::time::Duration::from_secs(seconds as u64));
    drop(stream); // stops recording

    let mono = buffer.lock().unwrap().clone();
    Ok(resample_to_16k(&mono, sample_rate))
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

// ---- App entry ----

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![transcribe, record_and_transcribe])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}