//! The last deliberate model choice, separate from handwritten settings.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct Selection {
    version: u32,
    pub(crate) provider: String,
    pub(crate) model: String,
}

impl Selection {
    pub(crate) fn new(provider: String, model: String) -> Self {
        Self {
            version: 1,
            provider,
            model,
        }
    }
}

pub(crate) fn load(path: &Path) -> Option<Selection> {
    let value: Selection = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (value.version == 1 && !value.provider.trim().is_empty() && !value.model.trim().is_empty())
        .then_some(value)
}

pub(crate) fn save(path: &Path, value: &Selection) -> std::io::Result<()> {
    let directory = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(directory)?;
    let bytes = serde_json::to_vec(value)?;
    let (temporary, mut file) = temporary(directory)?;
    let written = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    let published = written.and_then(|()| std::fs::rename(&temporary, path));
    if published.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    published
}

fn temporary(directory: &Path) -> std::io::Result<(std::path::PathBuf, std::fs::File)> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    loop {
        let n = WRITES.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!("last-model.{}.{n}.tmp", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}
