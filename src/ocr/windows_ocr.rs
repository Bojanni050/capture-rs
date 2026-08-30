//! Dunne laag rond `Windows.Media.Ocr`.
//!
//! Windows heeft een prima OCR-engine ingebouwd, dus we hoeven geen Tesseract
//! mee te leveren. De WinRT-objecten hier zijn niet gegarandeerd `Send`, dus
//! deze struct blijft binnen één thread (zie `ocr::OcrService`).

use anyhow::{Context, Result};
use image::RgbaImage;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;
use crate::com::ensure_mta;
use windows::core::HSTRING;


pub struct WindowsOcr {
    engine: OcrEngine,
    max_dim: u32,
    language: String,
}

/// Ruwe uitvoer van de engine: regels in leesvolgorde.
#[derive(Debug, Clone, Default)]
pub struct RawOcr {
    pub lines: Vec<String>,
}

impl WindowsOcr {
    /// `language` is een BCP-47-tag; `None` gebruikt je Windows-profieltalen.
    pub fn new(language: Option<&str>) -> Result<Self> {
        ensure_mta()?;

        let engine = match language {
            Some(tag) => {
                let lang = Language::CreateLanguage(&HSTRING::from(tag))
                    .with_context(|| format!("onbekende taal-tag: {tag}"))?;
                OcrEngine::TryCreateFromLanguage(&lang).with_context(|| {
                    format!(
                        "geen OCR-taalpakket voor {tag}; installeer het via \
                         Instellingen > Tijd en taal > Taal en regio"
                    )
                })?
            }
            None => OcrEngine::TryCreateFromUserProfileLanguages().context(
                "geen OCR-engine voor je profieltalen; installeer een taalpakket \
                 met OCR-ondersteuning",
            )?,
        };

        let language = engine
            .RecognizerLanguage()
            .and_then(|l| l.LanguageTag())
            .map(|t| t.to_string_lossy())
            .unwrap_or_else(|_| "onbekend".into());

        let max_dim = OcrEngine::MaxImageDimension().unwrap_or(10_000);

        Ok(Self {
            engine,
            max_dim,
            language,
        })
    }

    pub fn language(&self) -> &str {
        &self.language
    }

    /// Voert OCR uit op één frame.
    pub fn recognize(&self, image: &RgbaImage) -> Result<RawOcr> {
        let (w, h) = image.dimensions();
        if w == 0 || h == 0 {
            return Ok(RawOcr::default());
        }

        // De engine weigert afbeeldingen boven MaxImageDimension.
        let scaled;
        let source = if w > self.max_dim || h > self.max_dim {
            let ratio = self.max_dim as f32 / w.max(h) as f32;
            let nw = ((w as f32 * ratio) as u32).max(1);
            let nh = ((h as f32 * ratio) as u32).max(1);
            scaled = image::imageops::resize(image, nw, nh, image::imageops::FilterType::Triangle);
            &scaled
        } else {
            image
        };

        let bitmap = to_software_bitmap(source)?;
        let result = self
            .engine
            .RecognizeAsync(&bitmap)
            .context("OCR starten mislukt")?
            .join()
            .context("OCR afronden mislukt")?;

        let ocr_lines = result.Lines().context("OCR-regels ophalen mislukt")?;
        let count = ocr_lines.Size().unwrap_or(0);
        let mut lines = Vec::with_capacity(count as usize);

        for i in 0..count {
            let line = match ocr_lines.GetAt(i) {
                Ok(l) => l,
                Err(_) => continue,
            };
            if let Ok(text) = line.Text() {
                let text = text.to_string_lossy();
                if !text.trim().is_empty() {
                    lines.push(text);
                }
            }
        }

        Ok(RawOcr { lines })
    }
}

/// RGBA-frame naar een BGRA8 `SoftwareBitmap`, wat de OCR-engine verwacht.
fn to_software_bitmap(image: &RgbaImage) -> Result<SoftwareBitmap> {
    let (w, h) = image.dimensions();
    let raw = image.as_raw();

    let mut bgra = vec![0u8; raw.len()];
    for (dst, src) in bgra.as_chunks_mut::<4>().0.iter_mut().zip(raw.as_chunks::<4>().0) {
        dst[0] = src[2];
        dst[1] = src[1];
        dst[2] = src[0];
        // Screenshots dragen soms een nul-alfakanaal mee; dat zou de engine
        // een volledig transparant (dus leeg) beeld laten zien.
        dst[3] = 255;
    }

    let buffer = CryptographicBuffer::CreateFromByteArray(&bgra)
        .context("beeldbuffer aanmaken mislukt")?;
    SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, w as i32, h as i32)
        .context("SoftwareBitmap aanmaken mislukt")
}

/// Alle talen waarvoor een OCR-pakket geïnstalleerd is.
pub fn available_languages() -> Result<Vec<String>> {
    ensure_mta()?;
    let langs = OcrEngine::AvailableRecognizerLanguages().context("taallijst ophalen mislukt")?;
    let mut out = Vec::new();
    for i in 0..langs.Size().unwrap_or(0) {
        if let Ok(lang) = langs.GetAt(i)
            && let Ok(tag) = lang.LanguageTag()
        {
            out.push(tag.to_string_lossy());
        }
    }
    Ok(out)
}
