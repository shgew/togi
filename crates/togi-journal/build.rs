use std::error::Error;
use std::path::Path;

#[path = "build/fingerprint.rs"]
mod fingerprint;

fn main() -> Result<(), Box<dyn Error>> {
    let root = Path::new("../..");
    let files = fingerprint::reducer_files(root)?;
    for path in &files {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-env-changed=TOGI_REV");
    let identity = fingerprint::digest(root, &files, option_env!("TOGI_REV").unwrap_or("dev"))?;
    println!("cargo:rustc-env=TOGI_SNAPSHOT_BUILD={identity}");
    Ok(())
}
