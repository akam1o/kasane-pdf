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
    /// Maximum decoded size of a single stream (before and after each
    /// filter stage). Default: 256 MiB.
    pub max_stream_bytes: u64,
    /// Maximum cumulative decoded bytes across all streams decoded by a
    /// single [`Document`](crate::Document). Guards against many small
    /// streams exhausting memory. Default: 1 GiB.
    pub max_total_decoded_bytes: u64,
    /// Maximum number of filters in a `/Filter` array. Default: 8.
    pub max_filter_chain: usize,
    /// Maximum entries in a single object stream (`/N`). Default: 10,000.
    pub max_objstm_entries: usize,
    /// Maximum byte range of an object stream's object list (`/First` +
    /// header + body). Default: 64 MiB.
    pub max_objstm_bytes: usize,
    /// Maximum number of parsed content-stream operations per page.
    /// Default: 1,000,000.
    pub max_operations: usize,
    /// Maximum operands waiting for an operator in the content-stream
    /// operand stack. Default: 1,000.
    pub max_operand_stack: usize,
    /// Maximum `q` (gsave) stack depth in a content stream.
    /// Default: 1,024.
    pub max_gsave_depth: usize,
    /// Maximum rendered output dimension (width or height) in pixels.
    /// Default: 16,384.
    pub max_render_pixels: u32,
    /// Maximum total rendered pixels (width × height). Default: 64 Mi.
    pub max_render_total_pixels: u64,
    /// Maximum bytes for the rendered pixel buffer (RGBA8).
    /// Default: 256 MiB.
    pub max_render_buffer_bytes: u64,
    /// Maximum number of path construction elements (m/l/c/v/y/h/re
    /// segment endpoints) accumulated in one painting path. Guards
    /// against pathological path input. Default: 100,000.
    pub max_path_elements: usize,
    /// Maximum depth of `Do`-invoked Form XObjects (a Form that `Do`s
    /// another Form counts one level). Guards self-referential and
    /// deeply nested forms. Default: 32.
    pub max_xobject_depth: u32,
    /// Maximum total number of `Do` invocations processed while
    /// rendering one page (Forms and images together). Bounds sibling
    /// fan-out that a depth cap alone cannot stop. Default: 10,000.
    pub max_xobject_invocations: u64,
    /// Maximum width or height of an image XObject in samples.
    /// Default: 16,384.
    pub max_image_dimension: u32,
    /// Maximum total samples (width × height) of a single image.
    /// Default: 64 Mi.
    pub max_image_total_samples: u64,
    /// Maximum decoded bytes for a single image's sample data.
    /// Default: 256 MiB.
    pub max_image_bytes: u64,
    /// Maximum total character codes processed by text-showing
    /// operators on one page — `Tj`/`TJ`/`'`/`"` all count here, so a
    /// single huge string or many small strings both hit the cap.
    /// Default: 1,000,000.
    pub max_text_codes: u64,
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
            max_stream_bytes: 256 * 1024 * 1024,
            max_total_decoded_bytes: 1024 * 1024 * 1024,
            max_filter_chain: 8,
            max_objstm_entries: 10_000,
            max_objstm_bytes: 64 * 1024 * 1024,
            max_operations: 1_000_000,
            max_operand_stack: 1_000,
            max_gsave_depth: 1_024,
            max_render_pixels: 16_384,
            max_render_total_pixels: 67_108_864,
            max_render_buffer_bytes: 256 * 1024 * 1024,
            max_path_elements: 100_000,
            max_xobject_depth: 32,
            max_xobject_invocations: 10_000,
            max_image_dimension: 16_384,
            max_image_total_samples: 67_108_864,
            max_image_bytes: 256 * 1024 * 1024,
            max_text_codes: 1_000_000,
        }
    }
}
