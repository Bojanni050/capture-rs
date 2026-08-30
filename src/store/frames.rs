//! Frames op schijf: JPEG onder `frames/JJJJ/MM/DD/`.
//!
//! Alleen frames die de image-fallback nodig heeft (of die je expliciet wilt
//! bewaren) belanden hier. De database houdt het relatieve pad bij.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Datelike, Local, TimeZone};
use image::{RgbImage, RgbaImage};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub struct FrameStore {
    root: PathBuf,
    quality: u8,
    max_width: u32,
    counter: AtomicU64,
}

impl FrameStore {
    pub fn new(root: PathBuf, quality: u8, max_width: u32) -> Self {
        Self {
            root,
            quality: quality.clamp(1, 100),
            max_width,
            counter: AtomicU64::new(0),
        }
    }


    /// Slaat een frame op en geeft (relatief pad, breedte, hoogte) terug.
    pub fn save(&self, image: &RgbaImage, ts: i64) -> Result<(String, u32, u32)> {
        let scaled;
        let source = if self.max_width > 0 && image.width() > self.max_width {
            let ratio = self.max_width as f32 / image.width() as f32;
            let h = ((image.height() as f32 * ratio) as u32).max(1);
            scaled = image::imageops::resize(
                image,
                self.max_width,
                h,
                image::imageops::FilterType::Triangle,
            );
            &scaled
        } else {
            image
        };

        let (w, h) = source.dimensions();
        let rgb = to_rgb(source);

        let when: DateTime<Local> = Local
            .timestamp_opt(ts, 0)
            .single()
            .ok_or_else(|| anyhow!("ongeldig tijdstempel: {ts}"))?;

        let dir_rel = format!("{:04}/{:02}/{:02}", when.year(), when.month(), when.day());
        let dir = self.root.join(&dir_rel);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("framemap aanmaken mislukt: {}", dir.display()))?;

        let seq = self.counter.fetch_add(1, Ordering::Relaxed);
        let name = format!("{ts}-{seq:04}.jpg");
        let rel = format!("{dir_rel}/{name}");
        let full = dir.join(&name);

        let mut buf = Vec::new();
        let mut encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, self.quality);
        encoder
            .encode_image(&rgb)
            .context("frame coderen naar JPEG mislukt")?;

        std::fs::write(&full, &buf)
            .with_context(|| format!("frame schrijven mislukt: {}", full.display()))?;

        Ok((rel, w, h))
    }

    /// Leest een frame terug. Weigert paden die buiten de framemap wijzen.
    pub fn read(&self, rel: &str) -> Result<Vec<u8>> {
        let path = self.resolve(rel)?;
        std::fs::read(&path).with_context(|| format!("frame lezen mislukt: {}", path.display()))
    }

    pub fn delete(&self, rel: &str) -> Result<()> {
        let path = self.resolve(rel)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            // Al weg is ook goed.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("frame verwijderen mislukt: {}", path.display())),
        }
    }

    /// Verwijdert lege datummappen die na een purge zijn achtergebleven.
    pub fn prune_empty_dirs(&self) {
        // `prune` returnt `true` als een map leeg is en verwijderd is.
        // We willen nooit de root zelf verwijderen — alleen sub-mappen.
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let _ = prune(&path);
            }
        }
    }

    pub fn disk_usage(&self) -> u64 {
        usage(&self.root)
    }

    /// Zet een relatief pad om naar een absoluut pad binnen de framemap.
    ///
    /// De paden komen uit onze eigen database, maar ze reizen via een HTTP-API;
    /// een expliciete controle op `..` is goedkoop.
    fn resolve(&self, rel: &str) -> Result<PathBuf> {
        let rel = rel.replace('\\', "/");
        if rel.contains(':') || rel.contains('\0') || rel.contains("//") {
            return Err(anyhow!("ongeldig framepad: {rel}"));
        }
        if rel.split('/').any(|c| c == ".." || c == "." || c.is_empty()) {
            return Err(anyhow!("ongeldig framepad: {rel}"));
        }
        let joined = self.root.join(&rel);
        // `root.join` op Windows negeert root als `rel` absoluut is.
        // Extra check: resolved pad moet met root beginnen.
        if !joined.starts_with(&self.root) {
            return Err(anyhow!("ongeldig framepad (escape): {rel}"));
        }
        Ok(joined)
    }
}

fn to_rgb(source: &RgbaImage) -> RgbImage {
    let (w, h) = source.dimensions();
    let mut rgb = RgbImage::new(w, h);
    for (dst, src) in rgb
        .as_mut()
        .as_chunks_mut::<3>()
        .0
        .iter_mut()
        .zip(source.as_raw().as_chunks::<4>().0)
    {
        dst[0] = src[0];
        dst[1] = src[1];
        dst[2] = src[2];
    }
    rgb
}

fn usage(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0;
    for entry in entries.flatten() {
        match entry.file_type() {
            Ok(t) if t.is_dir() => total += usage(&entry.path()),
            Ok(_) => total += entry.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => {}
        }
    }
    total
}

/// Ruimt lege mappen op, van binnen naar buiten.
fn prune(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut empty = true;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !prune(&path) {
                empty = false;
            }
        } else {
            empty = false;
        }
    }
    if empty {
        let _ = std::fs::remove_dir(dir);
    }
    empty
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn store() -> (FrameStore, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        (FrameStore::new(dir.path().to_path_buf(), 70, 800), dir)
    }

    #[test]
    fn frame_opslaan_en_teruglezen() {
        let (store, _guard) = store();
        let img = RgbaImage::from_pixel(120, 90, Rgba([10, 120, 200, 255]));
        let (rel, w, h) = store.save(&img, 1_735_689_600).unwrap();

        assert_eq!((w, h), (120, 90));
        assert!(rel.ends_with(".jpg"), "{rel}");
        let bytes = store.read(&rel).unwrap();
        // JPEG begint altijd met de SOI-marker.
        assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn brede_frames_worden_geschaald() {
        let (store, _guard) = store();
        let img = RgbaImage::from_pixel(2400, 1200, Rgba([0, 0, 0, 255]));
        let (_, w, h) = store.save(&img, 1_735_689_600).unwrap();
        assert_eq!(w, 800);
        assert_eq!(h, 400);
    }

    #[test]
    fn padtraversal_wordt_geweigerd() {
        let (store, _guard) = store();
        assert!(store.read("../../geheim.txt").is_err());
        assert!(store.read("2026/../../etc").is_err());
    }

    #[test]
    fn verwijderen_van_iets_dat_er_niet_is_slaagt() {
        let (store, _guard) = store();
        assert!(store.delete("2026/01/01/bestaatniet.jpg").is_ok());
    }
}
