//! Download and verify one catalogue model into a directory (used by the macOS packaging script
//! to bundle the default model). Files already present with the right size are kept.
//!
//! ```sh
//! cargo run --release -p filmcraft-speech --features download --example fetch_model -- <models dir> [model id]
//! ```

#[cfg(feature = "download")]
fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(dir) = args.next() else {
        eprintln!("usage: fetch_model <models dir> [model id]");
        return std::process::ExitCode::from(2);
    };
    let id = args.next().unwrap_or_else(|| filmcraft_speech::models::DEFAULT_MODEL.to_string());
    let Some(m) = filmcraft_speech::models::find(&id) else {
        eprintln!("unknown model `{id}`");
        return std::process::ExitCode::from(2);
    };
    let dir = std::path::PathBuf::from(dir);
    let mut last = 0u64;
    let r = filmcraft_speech::models::download(&dir, m, &mut |done, total, name| {
        if done / (100 << 20) != last / (100 << 20) {
            eprintln!("{name}: {} / {} MB", done >> 20, total >> 20);
        }
        last = done;
        true
    });
    match r {
        Ok(()) if filmcraft_speech::models::installed(&dir, m) => {
            eprintln!("{id}: ready in {}", filmcraft_speech::models::model_dir(&dir, m).display());
            std::process::ExitCode::SUCCESS
        }
        Ok(()) => {
            eprintln!("{id}: incomplete after download");
            std::process::ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("{id}: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "download"))]
fn main() {}
