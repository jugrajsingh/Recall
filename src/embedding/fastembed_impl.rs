use std::sync::Mutex;

use anyhow::Result;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

const MODEL_ID: &str = "intfloat/multilingual-e5-small";

// `TextEmbedding::embed` takes `&mut self` (verified against fastembed 5.17.3
// docs.rs, which disagrees with the brief's assumed `&self`), but every other
// backend and every call site in this crate treats `EmbeddingProvider` as
// `&self`-only. A `Mutex` gives us that shape back without touching the public
// interface (Task 10.2) — callers never hold the provider across threads
// concurrently in practice (one instance per background worker / TUI search
// thread), so the lock is uncontended.
pub(crate) struct EmbeddingProvider {
    model: Mutex<TextEmbedding>,
}

impl EmbeddingProvider {
    pub(crate) fn new(show_progress: bool) -> Result<Self> {
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::MultilingualE5Small)
                .with_show_download_progress(show_progress),
        )?;
        Ok(Self { model: Mutex::new(model) })
    }

    pub(crate) fn embed_query(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        // e5 query prefix, matching the candle backend exactly (D4). fastembed
        // does not auto-prepend E5 prefixes (verified against docs.rs source),
        // so this is applied exactly once.
        let docs: Vec<String> = texts.iter().map(|t| format!("query: {t}")).collect();
        let mut model =
            self.model.lock().map_err(|_| anyhow::anyhow!("embedding model lock poisoned"))?;
        Ok(model.embed(docs, None)?)
    }

    pub(crate) fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let docs: Vec<String> = texts.iter().map(|t| format!("passage: {t}")).collect();
        let mut model =
            self.model.lock().map_err(|_| anyhow::anyhow!("embedding model lock poisoned"))?;
        Ok(model.embed(docs, None)?)
    }

    pub(crate) fn embed_documents_with_batch(
        &self,
        texts: &[String],
        batch_size: usize,
    ) -> Result<Vec<Vec<f32>>> {
        let docs: Vec<String> = texts.iter().map(|t| format!("passage: {t}")).collect();
        let mut model =
            self.model.lock().map_err(|_| anyhow::anyhow!("embedding model lock poisoned"))?;
        Ok(model.embed(docs, Some(batch_size.max(1)))?)
    }

    pub(crate) fn device_name(&self) -> &str {
        "CPU (ONNX)"
    }
}

pub(crate) fn availability_summary() -> String {
    format!("local fastembed {MODEL_ID} (ONNX Runtime, CPU)")
}
