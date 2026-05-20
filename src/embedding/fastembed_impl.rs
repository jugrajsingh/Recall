//! fastembed-based EmbeddingProvider implementation.
//!
//! Lightweight ONNX Runtime path. Selected when the `semantic-fastembed`
//! feature is enabled (default). No openssl chain, no candle, no Metal/CUDA
//! support — pure-Rust ONNX inference on CPU. Same public API as the candle
//! backend, so `src/embedding/mod.rs` can swap between them via cfg.
//!
//! Model: multilingual-e5-small (matches the candle backend choice).
//!
//! Ported from claude-history's fastembed integration (raine/claude-history).

use std::sync::Mutex;

use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

/// Embed-model selection. Kept identical to the candle backend so vec
/// embeddings stored in the DB remain comparable across builds.
const MODEL: EmbeddingModel = EmbeddingModel::MultilingualE5Small;
const DEFAULT_BATCH: usize = 32;

pub struct EmbeddingProvider {
    // fastembed's `embed` takes `&mut self`, but our crate's `EmbeddingProvider`
    // is passed around as `&EmbeddingProvider` to keep parity with the candle
    // backend (which can use `&self`). Mutex gives us interior mutability with
    // negligible overhead — embedding is single-threaded per worker anyway.
    model: Mutex<TextEmbedding>,
}

impl EmbeddingProvider {
    /// Initialise the embedder. On first run, fastembed downloads the ONNX
    /// model + tokenizer (~120 MB) to its default cache (under
    /// `~/.cache/fastembed/`). `show_progress` controls the download bar.
    pub fn new(show_progress: bool) -> Result<Self> {
        let opts = InitOptions::new(MODEL).with_show_download_progress(show_progress);
        let model = TextEmbedding::try_new(opts)
            .with_context(|| "failed to initialise fastembed (download or load error)")?;
        Ok(Self { model: Mutex::new(model) })
    }

    /// Embed query texts. Uses the e5 `query:` prefix convention — same as
    /// the candle backend so cached vector embeddings remain compatible.
    pub fn embed_query(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let prefixed: Vec<String> = texts.iter().map(|t| format!("query: {t}")).collect();
        let mut model =
            self.model.lock().map_err(|_| anyhow::anyhow!("fastembed mutex poisoned"))?;
        model.embed(prefixed, Some(DEFAULT_BATCH)).map_err(Into::into)
    }

    /// Embed document texts (the `passage:` prefix counterpart).
    pub fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let prefixed: Vec<String> = texts.iter().map(|t| format!("passage: {t}")).collect();
        let mut model =
            self.model.lock().map_err(|_| anyhow::anyhow!("fastembed mutex poisoned"))?;
        model.embed(prefixed, Some(DEFAULT_BATCH)).map_err(Into::into)
    }

    /// Embed documents in fixed-size batches. fastembed's internal batching
    /// already does this efficiently, but the candle backend has the same
    /// method so we mirror it.
    pub fn embed_documents_with_batch(
        &self,
        texts: &[String],
        batch_size: usize,
    ) -> Result<Vec<Vec<f32>>> {
        let prefixed: Vec<String> = texts.iter().map(|t| format!("passage: {t}")).collect();
        let mut model =
            self.model.lock().map_err(|_| anyhow::anyhow!("fastembed mutex poisoned"))?;
        model.embed(prefixed, Some(batch_size.max(1))).map_err(Into::into)
    }

    /// Human-readable device label. fastembed is CPU-only (ONNX Runtime
    /// has GPU paths but they require extra setup we don't ship today).
    pub fn device_name(&self) -> &str {
        "CPU (fastembed / ONNX)"
    }
}
