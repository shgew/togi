use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

fn sources(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            sources(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs")
            || path.file_name().is_some_and(|name| name == "Cargo.toml")
        {
            files.push(path);
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let root = Path::new("../..");
    let mut files = Vec::new();
    for name in [
        "togi-machine",
        "togi-journal",
        "togi-config",
        "togi-requests",
        "togi-tuner",
        "togi-defect",
        "togi-render",
        "togi-watch",
        "togi-session",
    ] {
        let directory = root.join("crates").join(name);
        if directory.is_dir() {
            sources(&directory, &mut files)?;
        }
    }
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "version.txt",
    ] {
        files.push(root.join(name));
    }
    files.sort();
    let mut digest = Sha256::new();
    for path in files {
        println!("cargo:rerun-if-changed={}", path.display());
        let name = path.strip_prefix(root)?.to_string_lossy();
        let bytes = fs::read(&path)?;
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(name.as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    println!("cargo:rerun-if-env-changed=TOGI_REV");
    digest.update(option_env!("TOGI_REV").unwrap_or("dev").as_bytes());
    let mut identity = String::with_capacity(64);
    for byte in digest.finalize() {
        write!(identity, "{byte:02x}")?;
    }
    println!("cargo:rustc-env=TOGI_SNAPSHOT_BUILD={identity}");
    Ok(())
}
