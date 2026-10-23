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
    /// Maximum `n g obj` definitions the recovery scan
    /// (`Document::from_bytes_recovering`) will register. Default:
    /// 1,048,576.
    pub max_recovery_objects: usize,
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
    /// Maximum cumulative UTF-8 bytes of resolved Unicode text stored
    /// in an [`crate::extract_text`] result for one page. Counted per
    /// `CharInfo` before it is pushed — a mapping repeated many times
    /// (split `Tj`s, `TJ` segments, repeated Form invocations) cannot
    /// evade the budget because each occurrence adds its own bytes.
    /// Default: 8 MiB.
    pub max_extracted_unicode_bytes: u64,
    /// Maximum indirect objects emitted by one [`crate::rewrite`] call.
    /// Bounds the old→new reference map and the output buffer together.
    /// Default: 200,000.
    pub max_output_objects: usize,
    /// Maximum total bytes emitted by one [`crate::rewrite`] call —
    /// checked *while* writing, so a huge output fails partway through
    /// without allocating the whole buffer first. Streams travel
    /// encoded, so the cap applies to encoded bytes.
    /// Default: 2 GiB.
    pub max_output_bytes: u64,
    /// Maximum depth of the reference-walk recursion inside
    /// [`crate::rewrite`]. Genuine indirect cycles (page → /Parent → …)
    /// terminate via the already-numbered map, not the depth cap — this
    /// bounds pathological *direct* container nesting (arrays of
    /// arrays of refs). Default: 512.
    pub max_rewrite_walk_depth: usize,
    /// Maximum annotations an [`crate::edit::Edit`] session may queue.
    /// Bounds the pending list before any object is materialised.
    /// Default: 10,000.
    pub max_edit_annots: u64,
    /// Maximum synthetic objects an [`crate::edit::Edit`] session may
    /// create (annotation dicts, appearance streams, copied `/Annots`
    /// arrays and edited page dicts). Each `/Square` costs three
    /// objects (annot + appearance + array) plus one edited page.
    /// Default: 40,000.
    pub max_edit_objects: u64,
    /// Maximum cumulative bytes of generated appearance-stream bodies
    /// an [`crate::edit::Edit`] session may hold. A `/Square`
    /// appearance is ~80 bytes; this cap stops an unbounded add loop
    /// from growing memory without limit. Default: 16 MiB.
    pub max_edit_annot_bytes: u64,

    /// Maximum `--rect` arguments one `diagnose_redaction` call accepts.
    /// Default: 256.
    pub max_diag_rects: u64,
    /// Maximum reported items (targets + uncertain + blockers) a single
    /// diagnosis may accumulate — bounds the result structure itself.
    /// Default: 100,000.
    pub max_diag_items: u64,
    /// Maximum *other* pages scanned for cross-page resource sharing —
    /// bounds the sharing analysis, not the page itself. Default: 32.
    pub max_diag_share_pages: u64,
    /// Maximum resource entries inspected while looking for cross-page
    /// sharing. Default: 100,000.
    pub max_diag_share_refs: u64,
    /// Maximum cumulative content-stream *executions* inside one
    /// `diagnose_redaction`/`remove_text` walk — every op `run()`
    /// dispatches counts once, whether it is a page-level op, an op
    /// inside a `Do`'d form, an empty `Tj`, or a state change. Bounds
    /// the `max_operations` (per-stream *parse*) ×
    /// `max_xobject_invocations` product to something practical: the
    /// same form invoked 10,000 times costs 10,000× its op count
    /// here. Default: 200,000.
    pub max_diag_exec_ops: u64,
    /// Maximum entries the `show_states` snapshot map may hold —
    /// one entry per page-level *or* form-level show op that ran,
    /// keyed by `(form_path, op_index)`. A snapshot is small (an
    /// `Rc`-shared font + four `f64`s + the key) but unbounded
    /// entries still grow linearly with executed show ops.
    /// Default: 200,000.
    pub max_show_states: u64,
    /// Maximum cumulative bytes of *generated* stream bodies one
    /// `remove_text`/`Edit` call may hold at once — the rewritten
    /// page `/Contents`, every cloned form body, and any other
    /// synthesized stream count against this while they sit in the
    /// overlay *before* `rewrite` emits the final PDF (which has its
    /// own `max_output_bytes`). Distinct from that output cap: a
    /// hundred small clones can fail here long before the writer
    /// limit is reached. Default: 64 MiB.
    pub max_edit_content_bytes: u64,

    /// Maximum findings one `audit_residuals` call may emit —
    /// further findings are dropped and an `AuditIncomplete` records
    /// the truncation. Default: 10,000.
    pub max_audit_findings: u64,
    /// Maximum reference edges followed during the audit's
    /// reachability walk — bounds the BFS over the object graph.
    /// Default: 1,000,000.
    pub max_audit_edges: u64,
    /// Maximum referrers recorded per object — each object's
    /// `referrers` list is capped so a heavily-referenced object
    /// cannot grow unbounded memory. Default: 8.
    pub max_audit_paths: u64,
    /// Maximum container depth followed while scanning one object
    /// for outgoing references (arrays of arrays of refs). Default:
    /// 64.
    pub max_audit_walk_depth: usize,
    /// Maximum container *elements* expanded across the audit's
    /// traversals — every array item / dict entry / name-tree node
    /// or pair examined. Separate from `max_audit_edges`, which
    /// counts only reference edges: a wide array of integers follows
    /// no edge yet still costs work. Default: 4,000,000.
    pub max_audit_elements: u64,
    /// Maximum content-stream operations scanned per page while
    /// looking for marked-content `/ActualText`/`/Alt`/`E`.
    /// Default: 200,000 (same order as `max_diag_exec_ops`).
    pub max_audit_content_ops: u64,
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
            max_recovery_objects: 1 << 20,
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
            max_extracted_unicode_bytes: 8 * 1024 * 1024,
            max_output_objects: 200_000,
            max_output_bytes: 2 * 1024 * 1024 * 1024,
            max_rewrite_walk_depth: 512,
            max_edit_annots: 10_000,
            max_edit_objects: 40_000,
            max_edit_annot_bytes: 16 * 1024 * 1024,
            max_diag_rects: 256,
            max_diag_items: 100_000,
            max_diag_share_pages: 32,
            max_diag_share_refs: 100_000,
            max_diag_exec_ops: 200_000,
            max_show_states: 200_000,
            max_edit_content_bytes: 64 * 1024 * 1024,
            max_audit_findings: 10_000,
            max_audit_edges: 1_000_000,
            max_audit_paths: 8,
            max_audit_walk_depth: 64,
            max_audit_elements: 4_000_000,
            max_audit_content_ops: 200_000,
        }
    }
}
