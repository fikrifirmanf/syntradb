//! Repository source fingerprint, independent of git and runtime working directory.
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fmt::Write;
use std::fs;
use std::path::{Component, Path, PathBuf};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub fn fingerprint_files(root: &Path, paths: &[PathBuf]) -> Result<String> {
    let mut paths = paths.to_vec();
    paths.sort();
    paths.dedup();
    let mut hash = Sha256::new();
    hash.update(b"syntra-source-v1\0");
    for path in paths {
        let mut current = root.to_path_buf();
        let mut names = Vec::new();
        for component in path.components() {
            let Component::Normal(name) = component else {
                return Err("source fingerprint paths must be relative without traversal".into());
            };
            names.push(name.to_str().ok_or("source paths must be UTF-8")?);
            current.push(name);
            if fs::symlink_metadata(&current)?.file_type().is_symlink() {
                return Err("source fingerprint does not follow symlinks".into());
            }
        }
        if names.is_empty() || !fs::metadata(&current)?.is_file() {
            return Err("source fingerprint requires regular files".into());
        }
        let name = names.join("/");
        let bytes = fs::read(&current)?;
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
    }
    let mut output = String::with_capacity(64);
    for byte in hash.finalize() {
        write!(&mut output, "{byte:02x}")?;
    }
    Ok(output)
}

#[cfg(not(test))]
fn collect(root: &Path, directory: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(root.join(directory))? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let relative = directory.join(entry.file_name());
        if kind.is_symlink() {
            return Err("source fingerprint does not accept symlinks in source directories".into());
        }
        if kind.is_dir() && entry.file_name() != "target" {
            collect(root, &relative, paths)?;
        } else if kind.is_file()
            && matches!(
                relative.extension().and_then(|ext| ext.to_str()),
                Some("rs" | "toml" | "sql" | "sh" | "control" | "bin")
            )
        {
            paths.push(relative);
        }
    }
    Ok(())
}

#[cfg(not(test))]
pub fn emit() -> Result<()> {
    let manifest =
        std::env::var_os("CARGO_MANIFEST_DIR").ok_or("missing Cargo manifest directory")?;
    let manifest = Path::new(&manifest);
    let root = manifest
        .parent()
        .and_then(Path::parent)
        .ok_or("missing repository root")?;
    let mut paths: Vec<PathBuf> = [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        ".cargo/config.toml",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    for directory in ["crates", "build-support", "scripts", "examples"] {
        collect(root, Path::new(directory), &mut paths)?;
        println!("cargo:rerun-if-changed={}", root.join(directory).display());
    }
    for path in &paths {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    println!(
        "cargo:rustc-env=SYNTRA_SOURCE_FINGERPRINT={}",
        fingerprint_files(root, &paths)?
    );
    Ok(())
}
