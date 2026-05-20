//! Embedding backend façade.
//!
//! Two implementations are available, picked at compile time via Cargo
//! features:
//!
//!   * `semantic-fastembed` — Rust-native ONNX Runtime path (default). Pure
//!     Rust deps, no openssl chain, ~5 MB smaller binary, CPU-only.
//!   * `semantic-candle`    — original candle + hf-hub + tokenizers path.
//!     Heavier, but supports Metal (macOS) and CUDA acceleration.
//!
//! Both expose an identical `EmbeddingProvider` struct so the rest of the
//! crate (search engine, semantic queue, TUI) is backend-agnostic.
//!
//! If both feature flags are set on the same build, fastembed wins — this
//! matches Cargo's "features are additive" expectation while keeping the
//! smaller backend as the safe default. A user who wants candle should
//! build with `--no-default-features --features semantic-search,semantic-candle`.

#[cfg(feature = "semantic-fastembed")]
mod fastembed_impl;
#[cfg(feature = "semantic-fastembed")]
pub use fastembed_impl::EmbeddingProvider;

#[cfg(all(feature = "semantic-candle", not(feature = "semantic-fastembed")))]
mod candle_impl;
#[cfg(all(feature = "semantic-candle", not(feature = "semantic-fastembed")))]
pub use candle_impl::EmbeddingProvider;

// Compile-time guard: when semantic-search is on, one of the two backends
// must be enabled too.
#[cfg(all(
    feature = "semantic-search",
    not(feature = "semantic-fastembed"),
    not(feature = "semantic-candle"),
))]
compile_error!(
    "feature \"semantic-search\" requires one of \"semantic-fastembed\" or \"semantic-candle\". \
     Build with default features (which enables semantic-fastembed) or pass \
     `--features semantic-search,semantic-candle` explicitly."
);

// Stub for mini builds — the type exists so the rest of the codebase can
// keep `EmbeddingProvider` in signatures, but it can never be successfully
// constructed. Every entry point that would call `new()` is already cfg-
// gated to no-op in mini builds (see `cmd_sync`, `cmd_tui`, `do_search`,
// `run_background_worker`, etc.), so this stub is purely for type
// compilation.
#[cfg(not(any(feature = "semantic-fastembed", feature = "semantic-candle")))]
pub struct EmbeddingProvider;

#[cfg(not(any(feature = "semantic-fastembed", feature = "semantic-candle")))]
impl EmbeddingProvider {
    pub fn new(_show_progress: bool) -> anyhow::Result<Self> {
        anyhow::bail!(
            "semantic search is not compiled into this build (recall-mini). \
             Install the full build with `cargo build --release --features semantic-search`."
        )
    }

    pub fn embed_query(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        unreachable!("EmbeddingProvider cannot be instantiated in mini builds")
    }

    pub fn embed_documents(&self, _texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        unreachable!("EmbeddingProvider cannot be instantiated in mini builds")
    }

    pub fn embed_documents_with_batch(
        &self,
        _texts: &[String],
        _batch_size: usize,
    ) -> anyhow::Result<Vec<Vec<f32>>> {
        unreachable!("EmbeddingProvider cannot be instantiated in mini builds")
    }

    pub fn device_name(&self) -> &str {
        "disabled (mini)"
    }
}
