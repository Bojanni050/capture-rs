//! OCR als achtergronddienst.
//!
//! De WinRT-engine leeft op één eigen thread; de pipeline stuurt frames
//! heen en antwoorden komen via een oneshot terug. Zo hoeven we geen
//! aannames te doen over `Send`/`Sync` van de COM-objecten, en blokkeert
//! zware OCR nooit de async runtime.

pub mod windows_ocr;

pub use windows_ocr::{available_languages, RawOcr};

use anyhow::{anyhow, Result};
use image::RgbaImage;
use tokio::sync::{mpsc, oneshot};
use windows_ocr::WindowsOcr;

struct Job {
    image: RgbaImage,
    reply: oneshot::Sender<Result<RawOcr>>,
}

pub struct OcrService {
    tx: mpsc::Sender<Job>,
    language: String,
}

impl OcrService {
    /// Start de OCR-thread. Faalt meteen als er geen taalpakket beschikbaar is,
    /// zodat `chronicle start` een duidelijke fout geeft in plaats van stil
    /// alles als afbeelding op te slaan.
    pub fn start(language: Option<String>) -> Result<Self> {
        let (tx, mut rx) = mpsc::channel::<Job>(2);
        let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<String>>();

        std::thread::Builder::new()
            .name("chronicle-ocr".into())
            .spawn(move || {
                let engine = match WindowsOcr::new(language.as_deref()) {
                    Ok(engine) => {
                        let _ = init_tx.send(Ok(engine.language().to_string()));
                        engine
                    }
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        return;
                    }
                };

                while let Some(job) = rx.blocking_recv() {
                    let out = engine.recognize(&job.image);
                    let _ = job.reply.send(out);
                }
            })?;

        let language = init_rx
            .recv()
            .map_err(|_| anyhow!("OCR-thread startte niet"))??;

        Ok(Self { tx, language })
    }

    /// Taal waarin de engine daadwerkelijk herkent.
    pub fn language(&self) -> &str {
        &self.language
    }

    pub async fn recognize(&self, image: RgbaImage) -> Result<RawOcr> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Job { image, reply })
            .await
            .map_err(|_| anyhow!("OCR-thread is gestopt"))?;
        rx.await.map_err(|_| anyhow!("OCR gaf geen antwoord"))?
    }
}
