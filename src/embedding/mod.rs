// Compile-time guard: the marker without a backend is a misconfiguration.
#[cfg(all(
    feature = "semantic-search",
    not(any(feature = "semantic-candle", feature = "semantic-fastembed"))
))]
compile_error!(
    "feature `semantic-search` requires a backend: enable `semantic-candle` or `semantic-fastembed`"
);

// fastembed wins when both backends are enabled.
#[cfg(feature = "semantic-fastembed")]
mod fastembed_impl;
#[cfg(feature = "semantic-fastembed")]
pub(crate) use fastembed_impl::{EmbeddingProvider, availability_summary};

#[cfg(all(feature = "semantic-candle", not(feature = "semantic-fastembed")))]
mod candle_impl;
#[cfg(all(feature = "semantic-candle", not(feature = "semantic-fastembed")))]
pub(crate) use candle_impl::{EmbeddingProvider, availability_summary};

/// Build-variant identifier for `recall info`. Always compiled, including mini.
pub(crate) const fn build_variant() -> &'static str {
    #[cfg(feature = "semantic-fastembed")]
    {
        "recall-fastembed"
    }
    #[cfg(all(feature = "semantic-candle", not(feature = "semantic-fastembed")))]
    {
        "recall-candle"
    }
    #[cfg(not(feature = "semantic-search"))]
    {
        "recall-mini"
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(all(feature = "semantic-candle", not(feature = "semantic-fastembed")))]
    fn test_should_report_candle_variant_when_default_features() {
        assert_eq!(super::build_variant(), "recall-candle");
    }

    #[test]
    #[cfg(not(feature = "semantic-search"))]
    fn test_should_report_mini_variant_when_no_backend() {
        assert_eq!(super::build_variant(), "recall-mini");
    }

    #[test]
    #[cfg(feature = "semantic-fastembed")]
    fn test_should_report_fastembed_variant_when_fastembed_enabled() {
        assert_eq!(super::build_variant(), "recall-fastembed");
    }
}
