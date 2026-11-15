//! Build tasks for the workspace.
//!
//! `cargo run -p xtask -- web <out dir> [--parity]` builds `vela-web` for
//! `wasm32-unknown-unknown` in release mode and writes the browser package
//! (`vela_web.js`, `vela_web_bg.wasm`, the JS snippets) into `<out dir>`.
//!
//! This does what the `wasm-bindgen` command-line tool does, through its
//! library, so the workspace needs no separately installed tool.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use wasm_bindgen_cli_support::Bindgen;

fn workspace_root() -> PathBuf {
    // crates/xtask → the workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn web(out_dir: &Path, parity: bool) -> Result<(), String> {
    let root = workspace_root();
    let mut build = Command::new(env!("CARGO"));
    build.current_dir(&root).args([
        "build",
        "-p",
        "vela-web",
        "--release",
        "--target",
        "wasm32-unknown-unknown",
    ]);
    if parity {
        build.args(["--features", "parity"]);
    }
    let status = build
        .status()
        .map_err(|e| format!("could not run cargo: {e}"))?;
    if !status.success() {
        return Err("building vela-web failed".to_string());
    }
    let wasm = root.join("target/wasm32-unknown-unknown/release/vela_web.wasm");
    let mut bindgen = Bindgen::new();
    bindgen
        .input_path(&wasm)
        .web(true)
        .map_err(|e| format!("{e:#}"))?
        // Keep the default `new URL('vela_web_bg.wasm', import.meta.url)`, so a page can call `init()` bare.
        .omit_default_module_path(false);
    bindgen.generate(out_dir).map_err(|e| format!("{e:#}"))?;
    println!("wrote {}", out_dir.display());
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("web") => match args.get(1).filter(|a| !a.starts_with("--")) {
            Some(out) => web(Path::new(out), args.iter().any(|a| a == "--parity")),
            None => Err("usage: xtask web <out dir> [--parity]".to_string()),
        },
        _ => Err("usage: xtask web <out dir> [--parity]".to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
