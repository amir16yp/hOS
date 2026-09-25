//! Cached image thumbnails, shared by the image viewer and the file browser.
//!
//! Previews live in `~/.hoswm/previews` as QOI images named after the source
//! path, its modification time and size, so an edited image never shows a
//! stale preview. `hos-image --preview` writes them; `hos-files` reads them.
use crate::{config, qoi, surface::Surface};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

/// Longest edge of a generated preview.
pub const SIZE: usize = 160;
/// Previews kept in the cache; the oldest beyond this are removed.
pub const KEEP: usize = 256;

pub fn directory() -> PathBuf {
    config::directory().join("previews")
}

/// Cache entry name for a source image at a given size.
pub fn name(source: &Path, size: usize) -> io::Result<String> {
    let meta = fs::metadata(source)?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos() as u64);
    let path = fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    // FNV-1a over the source identity: short, stable and dependency-free.
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for bytes in [
        path.as_os_str().as_encoded_bytes(),
        &modified.to_le_bytes(),
        &meta.len().to_le_bytes(),
        &(size as u64).to_le_bytes(),
    ] {
        for byte in bytes {
            hash = (hash ^ *byte as u64).wrapping_mul(0x100_0000_01b3);
        }
    }
    Ok(format!("{hash:016x}-{size}.qoi"))
}
pub fn path(source: &Path, size: usize) -> io::Result<PathBuf> {
    path_in(&directory(), source, size)
}
pub fn path_in(cache: &Path, source: &Path, size: usize) -> io::Result<PathBuf> {
    Ok(cache.join(name(source, size)?))
}
/// Dimensions that fit inside a square of `size` without enlarging.
pub fn fit(width: usize, height: usize, size: usize) -> (usize, usize) {
    let longest = width.max(height);
    if longest == 0 || longest <= size {
        return (width.max(1), height.max(1));
    }
    (
        (width * size / longest).max(1),
        (height * size / longest).max(1),
    )
}
/// The cached preview, if one was already generated for this exact file.
pub fn cached(source: &Path, size: usize) -> Option<qoi::Image> {
    cached_in(&directory(), source, size)
}
pub fn cached_in(cache: &Path, source: &Path, size: usize) -> Option<qoi::Image> {
    qoi::load(path_in(cache, source, size).ok()?).ok()
}
/// Decode the source image, scale it down and store it in the cache.
pub fn generate(source: &Path, size: usize) -> io::Result<qoi::Image> {
    generate_in(&directory(), source, size)
}
pub fn generate_in(cache: &Path, source: &Path, size: usize) -> io::Result<qoi::Image> {
    let image = qoi::load(source)?;
    let (width, height) = fit(image.width, image.height, size);
    let mut surface = Surface::new(width, height);
    surface.draw_image_smooth(0, 0, width as i32, height as i32, image.view());
    qoi::save_surface(&path_in(cache, source, size)?, &surface)?;
    let _ = prune_in(cache, KEEP);
    Ok(qoi::Image {
        width,
        height,
        pixels: surface.pixels().to_vec(),
    })
}
/// The cached preview, generating it if this file has not been seen before.
pub fn thumbnail(source: &Path, size: usize) -> io::Result<qoi::Image> {
    thumbnail_in(&directory(), source, size)
}
pub fn thumbnail_in(cache: &Path, source: &Path, size: usize) -> io::Result<qoi::Image> {
    match cached_in(cache, source, size) {
        Some(image) => Ok(image),
        None => generate_in(cache, source, size),
    }
}
/// Keep the cache bounded, discarding the least recently written entries.
pub fn prune(keep: usize) -> io::Result<()> {
    prune_in(&directory(), keep)
}
pub fn prune_in(cache: &Path, keep: usize) -> io::Result<()> {
    let mut entries: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(cache)?
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "qoi"))
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    if entries.len() <= keep {
        return Ok(());
    }
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in entries.into_iter().skip(keep) {
        let _ = fs::remove_file(path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dimensions_fit_without_enlarging() {
        assert_eq!(fit(400, 200, 100), (100, 50));
        assert_eq!(fit(200, 400, 100), (50, 100));
        assert_eq!(fit(40, 20, 100), (40, 20));
        assert_eq!(fit(1000, 1, 100), (100, 1), "thin images keep a pixel");
        assert_eq!(fit(0, 0, 100), (1, 1));
    }
    #[test]
    fn previews_are_generated_once_and_follow_the_source_file() {
        let dir = std::env::temp_dir().join(format!("hoswm-preview-{}", std::process::id()));
        let cache = dir.join("previews");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("image.qoi");
        let mut pixels = vec![0xff000000u32; 400 * 200];
        pixels[..400].fill(0xffffffff);
        qoi::save(&source, 400, 200, &pixels).unwrap();
        assert!(cached_in(&cache, &source, 100).is_none());
        let preview = thumbnail_in(&cache, &source, 100).unwrap();
        assert_eq!((preview.width, preview.height), (100, 50));
        let first = path_in(&cache, &source, 100).unwrap();
        assert!(first.exists());
        assert_eq!(cached_in(&cache, &source, 100).unwrap().pixels, preview.pixels);
        // Rewriting the source changes its identity, and so its cache entry.
        std::thread::sleep(std::time::Duration::from_millis(10));
        qoi::save(&source, 400, 200, &vec![0xff112233; 400 * 200]).unwrap();
        assert!(
            cached_in(&cache, &source, 100).is_none(),
            "an edited image is not served from the old entry"
        );
        assert_ne!(path_in(&cache, &source, 100).unwrap(), first);
        thumbnail_in(&cache, &source, 100).unwrap();
        assert!(fs::read_dir(&cache).unwrap().count() >= 2);
        // Pruning keeps the newest entries.
        prune_in(&cache, 1).unwrap();
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 1);
        assert!(cached_in(&cache, &source, 100).is_some());
        assert!(thumbnail_in(&cache, &dir.join("missing.qoi"), 100).is_err());
        // The default location follows the configuration directory.
        assert_eq!(directory(), config::directory().join("previews"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
