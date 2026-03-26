use std::io::Write;
use std::path::Path;

/// Atomic file write: write to tempfile in same directory, then rename.
/// Prevents partial writes on crash/Ctrl+C.
pub fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent dir"))?;

    std::fs::create_dir_all(dir)?;

    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(content.as_bytes())?;
    tmp.flush()?;

    // Preserve permissions
    if let Ok(metadata) = std::fs::metadata(path) {
        if let Err(e) = std::fs::set_permissions(tmp.path(), metadata.permissions()) {
            tracing::warn!("Failed to preserve permissions for {}: {}", path.display(), e);
        }
    }

    tmp.persist(path)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

    Ok(())
}
