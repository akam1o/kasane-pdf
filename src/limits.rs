//! Configurable resource limits guarding against hostile or corrupt input.
//!
//! Every potentially unbounded operation in the parser is bounded by one of
//! these limits. Exceeding a limit produces [`Error::LimitExceeded`]; the
//! parser never recurses or allocates without bound.

/// Resource limits applied while parsing a document.
///
/// The default values are generous enough for ordinary documents while
/// still bounding pathological input. Construct with
/// `Limits::default()` and adjust fields as needed.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Maximum input size in bytes (checked before parsing starts).
    /// Default: 512 MiB.
    pub max_input_bytes: usize,
    /// Maximum number of indirect references followed while resolving
    /// objects (each `n g R` hop counts once). Guards reference loops
    /// such as `5 0 obj -> 5 0 R`.
    /// Default: 1,000,000.
    pub max_reference_depth: u64,
    /// Maximum depth of the page tree recursion.
    /// Default: 128.
    pub max_page_tree_depth: u32,
    /// Maximum total number of nodes visited while walking the page tree.
    /// Guards inflated `/Count` values and huge Kids arrays.
    /// Default: 100,000.
    pub max_page_tree_nodes: u64,
    /// Maximum number of direct elements in a single array.
    /// Default: 1,000,000.
    pub max_array_elements: usize,
    /// Maximum number of direct entries in a single dictionary.
    /// Default: 100,000.
    pub max_dict_entries: usize,
    /// Maximum length in bytes of a single string or name object.
    /// Default: 64 MiB.
    pub max_string_bytes: usize,
    /// Maximum recursion depth for nested array/dictionary literals in a
    /// single object definition.
    /// Default: 512.
    pub max_nesting_depth: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_input_bytes: 512 * 1024 * 1024,
            max_reference_depth: 1_000_000,
            max_page_tree_depth: 128,
            max_page_tree_nodes: 100_000,
            max_array_elements: 1_000_000,
            max_dict_entries: 100_000,
            max_string_bytes: 64 * 1024 * 1024,
            max_nesting_depth: 512,
        }
    }
}
