#[tauri::command]
fn transcribe(path: String) -> Result<String, String> {
    // 👇 primeira linha, aqui dentro
    let model_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../models/ggml-large-v3-turbo.bin");

    // a seguir (o que tu escreves):
    //  1. ler o WAV com hound (usa o argumento `path`)
    //  2. converter os samples para f32
    //  3. correr o whisper com model_path
    //  4. concatenar os segmentos e devolver o String

    todo!() // apaga isto quando tiveres o corpo
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![transcribe])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
