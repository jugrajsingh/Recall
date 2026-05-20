pub mod adapters;
pub mod bench;
pub mod config;
pub mod db;
// embedding module is always present so the rest of the codebase can keep
// `EmbeddingProvider` in signatures without cfg-noise. In mini builds the
// type exists but can never be instantiated (new() always errs).
pub mod embedding;
pub mod semantic;
pub mod tui;
pub mod types;
pub mod utils;
