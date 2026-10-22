//! M17 — **experimental** scoped text removal.
//!
//! [`remove_text`] deletes the character codes whose *filled* ink
//! bounds land inside caller-supplied rectangles, rewriting the page's
//! `/Contents` so the remaining glyphs keep their original positions.
//! The output is a new, single-revision PDF produced through the same
//! overlay/`rewrite_overlay` pipeline the annotation editor uses —
//! the input `Document` and its bytes are never touched.
//!
//! **This is not a redaction feature.** It is an experiment in
//! *character-level content editing*: it does not paint over anything,
//! does not remove other occurrences of the same data (metadata,
//! annotations, structure tree, other pages, font internals), and does
//! not guarantee irreversibility. See the crate docs and
//! `docs/features/editing-text.md` for the full scope list.
//!
//! **M18 — one level of `/Form`.** A page-level `Do` whose target is
//! a `/Form` XObject is in scope: the invocation's inner text is
//! edited by cloning the form stream into a new object, registering
//! it under a fresh `/XObject` name (`KasFm{n}`) in the page's
//! effective resources, and rewriting that `Do` to the new name. Two
//! `Do`s of the same form are distinct invocations — each clones its
//! own copy, so the unedited call (and every other page sharing the
//! original) keeps the original object. Nested `Do`s (a `Do` inside
//! a form), images, `/Group`/`/OC`/`/Ref` forms, and every other
//! M17 refusal stay refused — the eligibility check is the walk's
//! `form_path` depth plus the existing `blocker`/`uncertain`/mode
//! rules.
//!
//! Scope — a page is *refused* (typed error) rather than half-edited
//! when any of these appear in its content or page dict:
//!
//! - `/Annots` — annotations are out of scope (the diagnosis sees
//!   them; removal does not reason about their content).
//! - Any `sh`, `gs`, Pattern colour space, `BI`/`ID`/`EI` inline
//!   image, `/ActualText`/`OC` marked content, or an `Image`/nested
//!   `Do` — the diagnosis already marks these; removal refuses on
//!   the first one rather than continue past unknown state.
//! - Any text-rendering mode other than `Tr 0` (fill) anywhere in
//!   the shown text — the eligibility check is page-level, not
//!   per-rect: an out-of-rect `Tr 3`/`Tr 1`/`Tr 2` occurrence still
//!   refuses the page. (`Tr 4–7` and `w = 0` hairlines are already
//!   `blocker`/`uncertain` in the diagnosis.) A `Tr` *set* but never
//!   used to show text is harmless — the mode only matters when a
//!   glyph is painted under it.
//! - Unsupported or unresolvable fonts, non-finite transforms, and
//!   any other construct that makes the diagnosis `incomplete`,
//!   `uncertain`, or `blocker`-carrying — those are refused, never
//!   silently treated as "nothing to remove".
//!
//! **Op-time state (M17.1).** The diagnosis walker records the text
//! state *at each page-level show op* (`Tj`/`TJ`/`'`/`"`) —
//! resolved font, `Tfs`, `Th`, `Tc`, `Tw` — keyed by `op_index`.
//! Selection and rewrite read the *same* snapshot, so a `Tf`/`Tc`/
//! `Tw`/`Tz` change, a `q`/`Q` restore, or a second `BT`/`ET` later
//! in the stream can never rewrite an earlier show under the wrong
//! parameters. `"`'s snapshot is taken *after* its `Tw`/`Tc` update
//! so the kern math sees the values the op actually used.
//!
//! Position preservation: each removed code is replaced by a `TJ`
//! numeric element whose value is the negation of that code's
//! *advance* — `(w0/1000 × Tfs + Tc [+ Tw if space]) × Th` — so the
//! following glyphs land exactly where they did before. `'` and `"`
//! are expanded into their `T*` + `Tj` (and `Tw`/`Tc`) equivalents
//! first so the rewrite never loses their side effects.

use crate::content::{Operation, TJItem};
use crate::document::Document;
use crate::edit::Edit;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, IndirectRef, Object};
use crate::redact_diag::{
    check_rect, page_dict_entry, DiagWalker, FormFrame, ShowTextState, Site, TargetDetail,
    TargetKind,
};
use std::collections::BTreeMap;

/// The result of [`remove_text`] — the rewritten PDF bytes plus the
/// audit trail of what was removed.
pub struct Removal {
    /// The new single-revision PDF.
    pub output: Vec<u8>,
    /// How many character codes were removed.
    pub codes_removed: u64,
    /// The `Site` of every removed code — page content ref + op
    /// index + code index inside the string (per `Site` docs).
    pub sites: Vec<Site>,
    /// Human-readable scope statement for the CLI/report — what was
    /// checked and what limits applied. Never contains the document
    /// text itself.
    pub scope: Vec<String>,
}

/// Terse summary for logging — never prints `output` bytes or site
/// content so a stray `{:?}` cannot leak document text.
impl std::fmt::Debug for Removal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Removal")
            .field("output_bytes", &self.output.len())
            .field("codes_removed", &self.codes_removed)
            .field("sites", &self.sites.len())
            .field("scope", &self.scope)
            .finish()
    }
}

/// Experimental scoped text removal — see the module docs for the
/// full refusal list. `rects` are in un-rotated, un-cropped user
/// space (+x right, +y up) — the same convention `diagnose_redaction`
/// and `extract_text` use.
///
/// On any failure — validation, an unsupported construct, or a
/// limit — nothing is emitted; a partial rewrite is never returned
/// as success.
pub fn remove_text(
    doc: &Document,
    page_index: usize,
    rects: &[(f64, f64, f64, f64)],
    limits: &Limits,
) -> Result<Removal, Error> {
    // ---- input validation (same rules as the diagnosis) -----------
    if rects.is_empty() {
        return Err(Error::Structure("at least one --rect is required".into()));
    }
    if rects.len() as u64 > limits.max_diag_rects {
        return Err(Error::LimitExceeded {
            limit: "max_diag_rects",
            max: limits.max_diag_rects,
        });
    }
    let mut validated = Vec::with_capacity(rects.len());
    for (i, r) in rects.iter().enumerate() {
        validated.push(check_rect(i, r.0, r.1, r.2, r.3)?);
    }
    let page = doc.pages().get(page_index).ok_or_else(|| {
        Error::Structure(format!(
            "page {} does not exist (document has {} pages)",
            page_index + 1,
            doc.page_count()
        ))
    })?;

    // ---- pre-flight refusal checks --------------------------------
    // /Annots — annotations are out of scope. Even though removal
    // does not touch them, their *content* is not something we reason
    // about, so a page carrying them is refused rather than edited
    // blind.
    if page_dict_entry(doc, page, b"Annots")?.is_some() {
        return Err(Error::Unsupported {
            feature: "text removal on a page with /Annots",
            detail: "annotations are out of scope — their content is \
                     not evaluated"
                .into(),
        });
    }
    // /UserUnit ≠ 1 changes user-space units — refuse as the
    // diagnosis does.
    if let Some(uu) = page_dict_entry(doc, page, b"UserUnit")? {
        match uu.as_number() {
            Some(v) if (v - 1.0).abs() > f64::EPSILON => {
                return Err(Error::Unsupported {
                    feature: "/UserUnit other than 1",
                    detail: "coordinates are not default units".into(),
                })
            }
            Some(_) => {}
            None => return Err(Error::Structure("/UserUnit is not a number".into())),
        }
    }

    // ---- walk the content ------------------------------------------
    // Reuse the diagnosis walker: it tracks graphics/text state the
    // same way the renderer does, reports a `Target` per shown code,
    // and flags every construct we refuse as `uncertain`/`blocker`
    // before we ever touch the stream.
    let ops = doc.page_operations(page)?;
    let mut walker = DiagWalker::new(doc, page, limits, &validated);
    // A mid-stream failure keeps its *typed* error — `LimitExceeded`
    // stays a limit, `Structure`/`Parse` stay input problems; only
    // constructs the scope does not cover surface as `Unsupported`
    // (those arrive via `blockers`/`uncertain`, not `Err`).
    walker.run(&ops)?;
    // Any blocker or uncertain entry means part of the page's state
    // or content was not fully understood — refuse rather than edit
    // past it.
    if let Some(b) = walker.blockers.first() {
        return Err(Error::Unsupported {
            feature: "text removal on a page with blockers",
            detail: format!("{}: {}", b.what, b.detail),
        });
    }
    if let Some(u) = walker.uncertain.first() {
        return Err(Error::Unsupported {
            feature: "text removal on a page with uncertain content",
            detail: u.reason.clone(),
        });
    }
    // `sh` is out of scope outright — shading fills an area we cannot
    // bound or rewrite around. Refuse on the first one.
    if let Some((i, _)) = ops
        .iter()
        .enumerate()
        .find(|(_, o)| matches!(o, Operation::Shading(_)))
    {
        return Err(Error::Unsupported {
            feature: "text removal on a page with sh",
            detail: format!("op #{i} — shading content is out of scope"),
        });
    }

    // ---- page-level eligibility -----------------------------------
    // A page is eligible only when every occurrence the walk produced
    // is one M18 can faithfully rewrite:
    //
    // - `Text` targets must all be `Tr 0` fill — page-level *and*
    //   form-level alike. An out-of-rect `Tr 3`/`Tr 1` glyph still
    //   refuses the page: eligibility is a property of the shown
    //   text, not of what a rect hit. (`Tr 4–7` and `w = 0` stroked
    //   text already landed in `blockers`/`uncertain` above.)
    // - `Form` targets are the `Do` invocations the page made —
    //   always at `form_path` depth 0.
    // - `Image` and `Other` targets are refused regardless of rects —
    //   their *presence* is out of scope.
    // - Any occurrence at `form_path` depth ≥ 2 means a `Do` inside
    //   a form — nested XObjects are out of scope for M18.
    for t in &walker.targets {
        if t.site.form_path.len() > 1 {
            return Err(Error::Unsupported {
                feature: "text removal on a page with nested XObjects",
                detail: "a Do inside a form is out of scope".into(),
            });
        }
        match t.kind {
            TargetKind::Text => {
                if let TargetDetail::Text { render_mode, .. } = &t.detail {
                    if *render_mode != 0u8 {
                        return Err(Error::Unsupported {
                            feature: "text removal on a page with non-fill text",
                            detail: format!(
                                "Tr {render_mode} text is out of scope — only Tr 0 fill \
                                 is rewritten (op #{}, code #{:?})",
                                t.site.op_index, t.site.code_index
                            ),
                        });
                    }
                }
            }
            // A `Form` target is a `Do` invocation record — at depth 0
            // it's the page-level call we may edit; at depth ≥ 1 it
            // is a `Do` *inside* a form → nested, refused. (The
            // `form_path.len() > 1` check above catches the inner
            // *text*; this catches an empty nested form.)
            TargetKind::Form if !t.site.form_path.is_empty() => {
                return Err(Error::Unsupported {
                    feature: "text removal on a page with nested XObjects",
                    detail: "a Do inside a form is out of scope".into(),
                });
            }
            TargetKind::Form | TargetKind::Path => {}
            TargetKind::Image | TargetKind::Other => {
                return Err(Error::Unsupported {
                    feature: "text removal on a page with non-form XObjects",
                    detail: format!(
                        "an {:?} occurrence at op #{} is out of scope",
                        t.kind, t.site.op_index
                    ),
                });
            }
        }
    }
    // (Any uncertain or blocker site inside a nested form was already
    // refused above — the depth check on `targets` covers the case
    // where the nested call produced only in-scope occurrences.)

    // ---- collect codes to remove ------------------------------------
    // ((form_path, op_index) → sorted code_index list). A code's
    // `hits` tell us its ink bounds overlapped a user rect — the
    // *filled* bounds of a `Tr 0` glyph, edge-touching counts as a
    // hit. The `form_path` is part of the key: two `Do` invocations of
    // the same form (or two names for the same object) are distinct
    // removal scopes because their CTMs differ. Every target reaching
    // here is already `Tr 0` (eligibility above).
    let mut to_remove: BTreeMap<(Vec<FormFrame>, usize), Vec<usize>> = BTreeMap::new();
    // Site lookup for the audit trail — the target's own `Site`
    // already carries the correct `form_path`/`op_index`/`code_index`,
    // so `removed_sites` does not reconstruct them.
    let mut site_lookup: BTreeMap<(Vec<FormFrame>, usize, usize), Site> = BTreeMap::new();
    for t in &walker.targets {
        if t.kind == TargetKind::Text && !t.hits.is_empty() {
            let ci = t
                .site
                .code_index
                .ok_or_else(|| Error::Structure("text target without code_index".into()))?;
            let key = (t.site.form_path.clone(), t.site.op_index);
            site_lookup
                .entry((t.site.form_path.clone(), t.site.op_index, ci))
                .or_insert_with(|| t.site.clone());
            to_remove.entry(key).or_default().push(ci);
        }
    }
    // Defensive: `binary_search` below needs sorted inputs — targets
    // arrive in code order already, but sorting is cheap insurance.
    for v in to_remove.values_mut() {
        v.sort_unstable();
    }

    // ---- clone forms for edited invocations -------------------------
    // (Deferred `Edit` — created lazily so a no-form-removal page
    // does not allocate an unused session.)
    let mut edit_guard: Option<Edit> = None;
    let mut do_renames: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    let mut removed_sites: Vec<Site> = Vec::new();
    let mut codes_removed: u64 = 0;
    // Cumulative bytes of *generated* stream bodies — each cloned
    // form and the new page `/Contents` count against
    // `max_edit_content_bytes` while they sit in the overlay,
    // *before* `rewrite` applies its own `max_output_bytes` to the
    // final PDF. Checked arithmetic; the overlay never holds more
    // than this budget at once.
    let mut generated_bytes: u64 = 0;
    // M18: each *invocation* whose inner text lost codes gets its own
    // cloned Form object — the original stream stays untouched so
    // other invocations (and other pages sharing it) keep painting the
    // removed characters. Cloning is per-invocation (no dedup): two
    // hits under different CTMs are different edits.
    let mut new_xobject_entries: Vec<(Vec<u8>, IndirectRef)> = Vec::new();
    // Collect the invocation keys first — iterating `to_remove` in
    // sorted order makes the generated names deterministic. Multiple
    // inner ops of the *same* invocation share one `form_path`, so
    // dedup before cloning.
    let mut seen_invocations: std::collections::BTreeSet<Vec<FormFrame>> =
        std::collections::BTreeSet::new();
    let form_invocations: Vec<Vec<FormFrame>> = to_remove
        .keys()
        .filter(|(fp, _)| fp.len() == 1)
        .map(|(fp, _)| fp.clone())
        .filter(|fp| seen_invocations.insert(fp.clone()))
        .collect();
    for (form_seq, fp) in form_invocations.iter().enumerate() {
        let frame = &fp[0];
        let do_op_index = frame.op_index;
        // Resolve the stream through the invocation's *name* in the
        // page resources — this covers both indirect and direct-dict
        // forms (`frame.xobj` is `None` for the latter). The name is
        // the one this `Do` used, so two names pointing at the same
        // object still produce per-invocation clones.
        let entry = doc
            .resolve_resource_raw(
                page.resources()
                    .ok_or_else(|| Error::Structure("page has no /Resources".into()))?,
                "XObject",
                &frame.name,
            )?
            .ok_or_else(|| {
                Error::Structure(format!(
                    "/XObject /{} no longer resolves",
                    String::from_utf8_lossy(&frame.name)
                ))
            })?;
        let obj = doc.deref(&entry)?;
        let (fdict, fraw) = obj
            .as_stream()
            .ok_or_else(|| Error::Structure("form XObject is not a stream".into()))?;
        let decoded = doc.decode_stream(fdict, fraw)?;
        let form_ops = crate::content::parse_content_stream(&[decoded], limits)?;
        // Rewrite the form's own ops — its `to_remove` entries are
        // keyed `(fp, inner_op_index)`.
        let mut new_form_ops: Vec<Operation> = Vec::with_capacity(form_ops.len());
        for (inner_idx, fop) in form_ops.iter().enumerate() {
            // A `Do`/`sh` inside the form is out of scope — the
            // eligibility scan already refused when the walker saw
            // one; this is a defensive re-check, never reached on a
            // passed page.
            if matches!(fop, Operation::Do(_) | Operation::Shading(_)) {
                return Err(Error::Unsupported {
                    feature: "text removal on a page with nested XObjects",
                    detail: "a Do/sh inside a form is out of scope".into(),
                });
            }
            new_form_ops.extend(rewrite_ops_for(
                &walker,
                &to_remove,
                &site_lookup,
                fp,
                inner_idx,
                fop,
                &mut codes_removed,
                &mut removed_sites,
            )?);
        }
        let new_body = serialize_ops(&new_form_ops, limits, generated_bytes)?;
        generated_bytes = charge_generated(generated_bytes, new_body.len(), limits)?;
        // Clone the stream dict, dropping `/Filter`/`/DecodeParms` —
        // the new body is emitted uncompressed, so the old filter
        // chain must not be claimed (a stale `/Filter` would decode
        // garbage on read). `/Length` is set from the new data.
        let mut new_dict = fdict.clone();
        new_dict.remove(b"Filter".as_slice());
        new_dict.remove(b"DecodeParms".as_slice());
        new_dict.remove(b"Length".as_slice());
        new_dict.insert(b"Length".to_vec(), Object::Integer(new_body.len() as i64));
        // The form may carry `/Group`, `/OC`, `/Ref` — the walker
        // already pushed blockers for them, so reaching here means
        // they are absent. `/Resources`, `/Matrix`, `/BBox` and every
        // other key are preserved verbatim.
        if edit_guard.is_none() {
            edit_guard = Some(Edit::new(doc, limits.clone())?);
        }
        let edit = edit_guard.as_mut().unwrap();
        let clone_ref = edit.alloc()?;
        edit.overlay.insert(
            (clone_ref.number, clone_ref.generation),
            Object::Stream {
                dict: new_dict,
                data: new_body,
            },
        );
        // Register the clone under a deterministic fresh name in the
        // page's `/XObject` dict, and rewrite the page-level `Do` to
        // it. `KasFm{n}` cannot collide with anything the page
        // resources already define (checked below before insert).
        let allocated: Vec<Vec<u8>> = new_xobject_entries.iter().map(|(n, _)| n.clone()).collect();
        let new_name = fresh_xobject_name(page, doc, form_seq as u64, &allocated)?;
        do_renames.insert(do_op_index, new_name.clone());
        new_xobject_entries.push((new_name, clone_ref));
    }

    // ---- rewrite the page content stream ----------------------------
    // Build a new ops vec with text ops rewritten; everything else
    // passes through unchanged. `Tj`/`'`/`"` are converted to `TJ`
    // when they lose codes so the survivors' positions are preserved.
    // Each op's rewrite uses the *state snapshot* the walker recorded
    // at that op (`show_states`), not the end-of-stream state — a
    // later `Tf`/`Tc`/`Tz` must not rewrite an earlier show.
    // `Do` ops whose invocation lost codes point at the new resource
    // names assigned above (`do_renames`).
    let mut new_ops: Vec<Operation> = Vec::with_capacity(ops.len());
    for (op_index, op) in ops.iter().enumerate() {
        match op {
            Operation::Do(_) => {
                if let Some(new_name) = do_renames.get(&op_index) {
                    new_ops.push(Operation::Do(new_name.clone()));
                } else {
                    new_ops.push(op.clone());
                }
            }
            _ => new_ops.extend(rewrite_ops_for(
                &walker,
                &to_remove,
                &site_lookup,
                &[],
                op_index,
                op,
                &mut codes_removed,
                &mut removed_sites,
            )?),
        }
    }

    // ---- serialise the new page stream -------------------------------
    let new_body = serialize_ops(&new_ops, limits, generated_bytes)?;
    charge_generated(generated_bytes, new_body.len(), limits)?;
    let mut edit = match edit_guard {
        Some(e) => e,
        None => Edit::new(doc, limits.clone())?,
    };
    let stream_ref = edit.alloc()?;
    edit.overlay.insert(
        (stream_ref.number, stream_ref.generation),
        Object::Stream {
            dict: {
                let mut d = crate::object::Dict::new();
                d.insert(b"Length".to_vec(), Object::Integer(new_body.len() as i64));
                d
            },
            data: new_body,
        },
    );
    let page_ref = page.object_ref().ok_or_else(|| {
        Error::Structure("cannot edit a page that has no indirect reference".into())
    })?;
    let page_obj = doc.resolve(page_ref)?;
    let mut page_dict = page_obj
        .as_dict()
        .cloned()
        .ok_or_else(|| Error::Structure(format!("page {page_ref} is not a dictionary")))?;
    page_dict.insert(b"Contents".to_vec(), Object::Ref(stream_ref));
    if !new_xobject_entries.is_empty() {
        // Merge the cloned forms into the page's *effective* `/XObject`
        // dict — the effective dict is materialised into the page's
        // `/Resources` so an inherited or shared resources object is
        // never mutated. Other categories (`/Font`, `/ExtGState`, …)
        // pass through verbatim (refs preserved).
        let mut res_dict = match page.resources() {
            Some(r) => doc
                .deref(r)?
                .as_dict()
                .cloned()
                .ok_or_else(|| Error::Structure("/Resources is not a dictionary".into()))?,
            None => Dict::new(),
        };
        // The `/XObject` entry may itself be a `Ref` to a shared dict
        // — deref + clone so the shared object is never mutated.
        let mut xobj_dict = match res_dict.get(b"XObject".as_slice()) {
            Some(o) => doc.deref(o)?.as_dict().cloned().ok_or_else(|| {
                Error::Structure("/Resources /XObject is not a dictionary".into())
            })?,
            None => Dict::new(),
        };
        for (name, r) in &new_xobject_entries {
            // `fresh_xobject_name` already verified the name is free.
            xobj_dict.insert(name.clone(), Object::Ref(*r));
        }
        res_dict.insert(b"XObject".to_vec(), Object::Dict(xobj_dict));
        page_dict.insert(b"Resources".to_vec(), Object::Dict(res_dict));
    }
    edit.overlay.insert(
        (page_ref.number, page_ref.generation),
        Object::Dict(page_dict),
    );
    let output = edit.rewrite()?;

    Ok(Removal {
        output,
        codes_removed,
        sites: removed_sites,
        scope: vec![
            format!(
                "page {} ({} rect{})",
                page_index + 1,
                validated.len(),
                if validated.len() == 1 { "" } else { "s" }
            ),
            format!("codes removed: {codes_removed}"),
            "scope: page-level content + one level of /Form — no nested XObjects, \
             no /Annots/metadata/structure"
                .into(),
        ],
    })
}

/// Fetch the text-state snapshot the walker recorded at
/// `(form_path, op_index)` — the state the show op *actually ran
/// under*. A missing entry means the op never executed its show
/// (impossible for an op the walker completed) or the walk diverged
/// from the ops list — either way it's a structure error, never a
/// guess.
///
/// Shared with `remove_region.rs` (M20).
pub(crate) fn show_state<'a>(
    walker: &'a DiagWalker<'a>,
    form_path: &[FormFrame],
    op_index: usize,
) -> Result<&'a ShowTextState, Error> {
    walker
        .show_states
        .get(&(form_path.to_vec(), op_index))
        .ok_or_else(|| {
            Error::Structure(format!(
                "no text-state snapshot for op #{op_index} (form_path depth {})",
                form_path.len()
            ))
        })
}

/// Rewrite a single `Tj`/`TJ`/`'`/`"` op at `(form_path, op_index)`
/// against its removal list and state snapshot — shared by the
/// page-level loop and the per-form clone. Returns the replacement
/// ops (0–3 for `'`/`"` expansion, 1 otherwise) and appends to
/// `codes_removed`/`removed_sites`.
///
/// A `Do`/`sh` or a non-show op passes straight through — callers
/// intercept `Do` before calling this.
///
/// Shared with `remove_region.rs` (M20).
#[allow(clippy::too_many_arguments)]
pub(crate) fn rewrite_ops_for(
    walker: &DiagWalker,
    to_remove: &BTreeMap<(Vec<FormFrame>, usize), Vec<usize>>,
    site_lookup: &BTreeMap<(Vec<FormFrame>, usize, usize), Site>,
    form_path: &[FormFrame],
    op_index: usize,
    op: &Operation,
    codes_removed: &mut u64,
    removed_sites: &mut Vec<Site>,
) -> Result<Vec<Operation>, Error> {
    let key = (form_path.to_vec(), op_index);
    let rem = to_remove.get(&key);
    match (op, rem) {
        (Operation::ShowText(bytes), Some(idxs)) => {
            let st = show_state(walker, form_path, op_index)?;
            let (new_items, n, sites) =
                rewrite_show_text(st, site_lookup, form_path, bytes, idxs, op_index)?;
            *codes_removed += n;
            removed_sites.extend(sites);
            Ok(new_items)
        }
        (Operation::ShowTextAdjusted(items), Some(idxs)) => {
            let st = show_state(walker, form_path, op_index)?;
            let (new_items, n, sites) =
                rewrite_tj(st, site_lookup, form_path, items, idxs, op_index)?;
            *codes_removed += n;
            removed_sites.extend(sites);
            Ok(new_items)
        }
        // `'` ≡ T* + Tj — emit the T* unconditionally so the line move
        // survives an empty Tj.
        (Operation::NextLineShowText(text), Some(idxs)) => {
            let st = show_state(walker, form_path, op_index)?;
            let (new_items, n, sites) =
                rewrite_show_text(st, site_lookup, form_path, text, idxs, op_index)?;
            *codes_removed += n;
            removed_sites.extend(sites);
            let mut v = vec![Operation::NextLine];
            v.extend(new_items);
            Ok(v)
        }
        // `"` ≡ Tw + Tc + T* + Tj — emit the spacing and T*
        // unconditionally. The snapshot already carries the op's *new*
        // `Tw`/`Tc` (recorded after the update).
        (
            Operation::SetSpacingNextLineShowText {
                word_spacing,
                char_spacing,
                text,
            },
            Some(idxs),
        ) => {
            let st = show_state(walker, form_path, op_index)?;
            let (new_items, n, sites) =
                rewrite_show_text(st, site_lookup, form_path, text, idxs, op_index)?;
            *codes_removed += n;
            removed_sites.extend(sites);
            let mut v = vec![
                Operation::SetWordSpacing(*word_spacing),
                Operation::SetCharSpacing(*char_spacing),
                Operation::NextLine,
            ];
            v.extend(new_items);
            Ok(v)
        }
        _ => Ok(vec![op.clone()]),
    }
}

/// Pick a fresh `/XObject` resource name for a cloned form —
/// deterministic (`KasFm0`, `KasFm1`, …) and verified absent from both
/// the page's effective `/XObject` dict *and* the names already
/// allocated this call (`allocated`), so a generated name can never
/// shadow a resource another `Do` uses or collide with a sibling
/// clone.
fn fresh_xobject_name(
    page: &crate::page::Page,
    doc: &Document,
    seq: u64,
    allocated: &[Vec<u8>],
) -> Result<Vec<u8>, Error> {
    let existing: Option<Dict> = match page.resources() {
        Some(r) => match doc.deref(r)?.as_dict() {
            Some(d) => match d.get(b"XObject".as_slice()) {
                Some(o) => Some(doc.deref(o)?.as_dict().cloned().ok_or_else(|| {
                    Error::Structure("/Resources /XObject is not a dictionary".into())
                })?),
                None => None,
            },
            None => return Err(Error::Structure("/Resources is not a dictionary".into())),
        },
        None => None,
    };
    for n in seq.. {
        let name = format!("KasFm{n}").into_bytes();
        let taken = existing
            .as_ref()
            .map(|d| d.contains_key(name.as_slice()))
            .unwrap_or(false)
            || allocated.contains(&name);
        if !taken {
            return Ok(name);
        }
        // Practically unreachable — a document would need `KasFm{n}`
        // for every n up to u64::MAX — but the loop is bounded so a
        // pathological dict can't spin forever.
        if n - seq > 1_000_000 {
            return Err(Error::LimitExceeded {
                limit: "xobject_name_space",
                max: 1_000_000,
            });
        }
    }
    unreachable!()
}

/// Rewrite one `Tj`/`'`/`"` string with `remove` codes taken out —
/// emits a `TJ` op (or nothing when every code went) whose numeric
/// elements carry each removed code's advance so following glyphs
/// land where they did. `st` is the *op-time* text state;
/// `site_lookup` supplies the removed codes' `Site`s (carrying the
/// right `form_path`).
///
/// Returns `(ops, codes_removed, sites)` — `ops` is empty when the
/// whole string was removed.
///
/// Shared with `remove_region.rs` (M20).
fn rewrite_show_text(
    st: &ShowTextState,
    site_lookup: &BTreeMap<(Vec<FormFrame>, usize, usize), Site>,
    form_path: &[FormFrame],
    bytes: &[u8],
    remove: &[usize],
    op_index: usize,
) -> Result<(Vec<Operation>, u64, Vec<Site>), Error> {
    let font = st
        .font
        .as_ref()
        .ok_or_else(|| Error::Structure("Tj before Tf — no font selected".into()))?;
    // The advance a removed code contributes is
    //   (w0/1000 × Tfs + Tc [+ Tw if space]) × Th
    // which a TJ numeric element reproduces as `-advance × 1000 /
    // (Tfs × Th)` — i.e. -(w0 + (Tc[+Tw])×1000/Tfs) — so the
    // surviving codes keep their original positions.
    let font_size = st.font_size;
    let h_scale = st.h_scale;
    if !(font_size.is_finite() && font_size > 0.0) {
        return Err(Error::Unsupported {
            feature: "text removal with Tfs = 0",
            detail: "cannot express the removed advance as a TJ number".into(),
        });
    }
    if !(h_scale.is_finite() && h_scale != 0.0) {
        return Err(Error::Unsupported {
            feature: "text removal with Tz = 0",
            detail: "cannot express the removed advance as a TJ number".into(),
        });
    }
    // `st.word_spacing`/`st.char_spacing` are the values the op ran
    // under — for `"` the snapshot already carries the op's new
    // spacing, so no separate handling is needed.
    let char_spacing = st.char_spacing;
    let word_spacing = st.word_spacing;

    // Decode every code so we can count positions and know widths.
    let mut codes: Vec<(u32, usize)> = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let (c, len) = font.read_code(bytes, pos)?;
        codes.push((c, pos));
        pos += len;
    }
    // `pos` ends at bytes.len() — the last code's `len` isn't stored
    // since we only need each code's *start* to rebuild strings.
    let _ = pos;

    // Split `codes` into kept runs and removed runs — each kept run
    // becomes a string element, each removed run a numeric element
    // carrying its summed advance. `byte_pos` inside `bytes` is
    // tracked so each kept string is the original byte slice, and the
    // numeric element goes *between* the kept runs so the surviving
    // codes land where they did: `(ABC)` removing 'B' →
    // `[(A) <advB> (C)]`, not `[(A)(C) <advB>]`.
    //
    // A removed code's advance (user space) is
    //   ((w0/1000)·Tfs + Tc [+Tw if space]) · Th
    // and a TJ numeric `n` moves the next glyph by `-n·Tfs·Th/1000`.
    // So `n = -(w0 + (Tc[+Tw])·1000/Tfs)` — `h_scale` cancels and must
    // not appear in the formula (it is *not* part of the TJ-number
    // semantics; §9.4.4 shows the adjustment is `w0 - n/1000` in glyph
    // space before `Tfs·Th` is applied). `st` carries the *op-time*
    // Tfs/Tc/Tw/Tz — a later state change never reaches this math.
    let mut items: Vec<TJItem> = Vec::new();
    let mut sites: Vec<Site> = Vec::new();
    let mut codes_removed: u64 = 0;
    let mut run_start: Option<usize> = None;
    let mut removed_advance = 0.0f64;
    for (ci, (code, byte_pos)) in codes.iter().enumerate() {
        // `remove` is sorted ascending (code_index order) — binary
        // search keeps the per-code test O(log n) rather than O(n)
        // so a long string with many removals is not quadratic.
        if remove.binary_search(&ci).is_ok() {
            // Removed — close any kept run *before* the numeric item
            // so the order is text, adjustment, text.
            if let Some(start) = run_start.take() {
                items.push(TJItem::Text(bytes[start..*byte_pos].to_vec()));
            }
            let w0 = font.width(*code);
            let tw = if font.is_space_code(*code) {
                word_spacing
            } else {
                0.0
            };
            removed_advance += w0 + (char_spacing + tw) * 1000.0 / font_size;
            codes_removed += 1;
            sites.push(
                site_lookup
                    .get(&(form_path.to_vec(), op_index, ci))
                    .cloned()
                    .ok_or_else(|| {
                        Error::Structure(format!("no site for removed code {ci} at op #{op_index}"))
                    })?,
            );
        } else {
            // Kept — but if a removed run ended just before, flush its
            // accumulated advance *first* so the kern sits between the
            // strings.
            if removed_advance != 0.0 {
                if !removed_advance.is_finite() {
                    return Err(Error::Unsupported {
                        feature: "text removal with a non-finite advance",
                        detail: "the accumulated removed advance is not finite".into(),
                    });
                }
                items.push(TJItem::Kern(-removed_advance));
                removed_advance = 0.0;
            }
            if run_start.is_none() {
                run_start = Some(*byte_pos);
            }
        }
    }
    if let Some(start) = run_start.take() {
        items.push(TJItem::Text(bytes[start..].to_vec()));
    }
    // A trailing removed run (string ends with removed codes) still
    // carries its advance — a following op's first glyph would land
    // where the removed text's end was.
    if removed_advance != 0.0 {
        if !removed_advance.is_finite() {
            return Err(Error::Unsupported {
                feature: "text removal with a non-finite advance",
                detail: "the accumulated removed advance is not finite".into(),
            });
        }
        items.push(TJItem::Kern(-removed_advance));
    }
    if items.is_empty() {
        return Ok((Vec::new(), codes_removed, sites));
    }
    Ok((
        vec![Operation::ShowTextAdjusted(items)],
        codes_removed,
        sites,
    ))
}

/// Rewrite a `TJ` array — each `Text` element is filtered with
/// `rewrite_show_text`'s logic but without the `code_index` reset
/// (the index runs across the whole array); numeric elements pass
/// through unchanged. `st` is the op-time text state.
///
/// Shared with `remove_region.rs` (M20).
fn rewrite_tj(
    st: &ShowTextState,
    site_lookup: &BTreeMap<(Vec<FormFrame>, usize, usize), Site>,
    form_path: &[FormFrame],
    items: &[TJItem],
    remove: &[usize],
    op_index: usize,
) -> Result<(Vec<Operation>, u64, Vec<Site>), Error> {
    let font = st
        .font
        .as_ref()
        .ok_or_else(|| Error::Structure("TJ before Tf — no font selected".into()))?;
    let font_size = st.font_size;
    let h_scale = st.h_scale;
    if !(font_size.is_finite() && font_size > 0.0) {
        return Err(Error::Unsupported {
            feature: "text removal with Tfs = 0",
            detail: "cannot express the removed advance as a TJ number".into(),
        });
    }
    if !(h_scale.is_finite() && h_scale != 0.0) {
        return Err(Error::Unsupported {
            feature: "text removal with Tz = 0",
            detail: "cannot express the removed advance as a TJ number".into(),
        });
    }
    let char_spacing = st.char_spacing;
    let word_spacing = st.word_spacing;

    let mut new_items: Vec<TJItem> = Vec::new();
    let mut sites: Vec<Site> = Vec::new();
    let mut codes_removed: u64 = 0;
    let mut ci: usize = 0; // code_index across the whole TJ array
    for item in items {
        match item {
            TJItem::Kern(n) => new_items.push(TJItem::Kern(*n)),
            TJItem::Text(bytes) => {
                // Decode this element's codes; each keeps or removes.
                let mut pos = 0;
                let mut codes: Vec<(u32, usize)> = Vec::new();
                while pos < bytes.len() {
                    let (c, len) = font.read_code(bytes, pos)?;
                    codes.push((c, pos));
                    pos += len;
                }
                // `removed_advance` is local to this Text element —
                // it flushes as a kern inside the element (before the
                // next kept run, or trailing at element end) so the
                // adjustment sits exactly where the codes were removed
                // and never crosses an original numeric element.
                let mut run_start: Option<usize> = None;
                let mut removed_advance = 0.0f64;
                for (local_ci, (code, byte_pos)) in codes.iter().enumerate() {
                    let global = ci + local_ci;
                    // `remove` is sorted ascending — binary search
                    // keeps this O(log n) per code.
                    if remove.binary_search(&global).is_ok() {
                        // Close the kept run *before* the kern so the
                        // order is text, adjustment, text.
                        if let Some(start) = run_start.take() {
                            new_items.push(TJItem::Text(bytes[start..*byte_pos].to_vec()));
                        }
                        let w0 = font.width(*code);
                        let tw = if font.is_space_code(*code) {
                            word_spacing
                        } else {
                            0.0
                        };
                        removed_advance += w0 + (char_spacing + tw) * 1000.0 / font_size;
                        codes_removed += 1;
                        sites.push(
                            site_lookup
                                .get(&(form_path.to_vec(), op_index, global))
                                .cloned()
                                .ok_or_else(|| {
                                    Error::Structure(format!(
                                        "no site for removed code {global} at op #{op_index}"
                                    ))
                                })?,
                        );
                    } else {
                        // Kept — flush any pending removed advance
                        // first so the kern lands between strings.
                        if removed_advance != 0.0 {
                            if !removed_advance.is_finite() {
                                return Err(Error::Unsupported {
                                    feature: "text removal with a non-finite advance",
                                    detail: "the accumulated removed advance is not finite".into(),
                                });
                            }
                            new_items.push(TJItem::Kern(-removed_advance));
                            removed_advance = 0.0;
                        }
                        if run_start.is_none() {
                            run_start = Some(*byte_pos);
                        }
                    }
                }
                if let Some(start) = run_start.take() {
                    new_items.push(TJItem::Text(bytes[start..].to_vec()));
                }
                // A removed run at the *end* of this element still
                // carries its advance — flush it inside the element so
                // `[(AB) 50 (C)]` removing 'B' gives
                // `[(A) -600 50 (C)]`, and a removal at array end gives
                // a trailing kern for the following op.
                if removed_advance != 0.0 {
                    if !removed_advance.is_finite() {
                        return Err(Error::Unsupported {
                            feature: "text removal with a non-finite advance",
                            detail: "the accumulated removed advance is not finite".into(),
                        });
                    }
                    new_items.push(TJItem::Kern(-removed_advance));
                }
                ci += codes.len();
            }
        }
    }
    if new_items.is_empty() {
        return Ok((Vec::new(), codes_removed, sites));
    }
    Ok((
        vec![Operation::ShowTextAdjusted(new_items)],
        codes_removed,
        sites,
    ))
}

/// Bounded output buffer — every `push`/`extend_from_slice` checks
/// the *remaining* byte budget *before* appending, so a long string
/// or huge TJ array stops mid-write rather than allocating past the
/// limit. `max_edit_content_bytes` (cumulative) and
/// `max_output_bytes` (single-stream) are enforced together; the
/// tighter of the two is hit first.
///
/// The buffer owns a `Vec<u8>` and never exposes it mutably — callers
/// cannot bypass the checks. On failure the partial bytes are
/// dropped with the buffer (no halfway stream escapes to the caller).
///
/// Shared with `remove_image.rs` (M19) and `remove_region.rs` (M20) —
/// the writer is agnostic to *what* it writes.
pub(crate) struct BoundedWriter<'a> {
    buf: Vec<u8>,
    limits: &'a Limits,
    /// Bytes already charged against `max_edit_content_bytes` by
    /// earlier streams in the same call — the remaining cumulative
    /// budget is `max_edit_content_bytes - generated_so_far`.
    generated_so_far: u64,
}

impl<'a> BoundedWriter<'a> {
    pub(crate) fn new(limits: &'a Limits, generated_so_far: u64) -> Self {
        Self {
            buf: Vec::new(),
            limits,
            generated_so_far,
        }
    }

    /// Check `n` more bytes fit before appending. Returns the
    /// specific limit that would be exceeded, or `Ok(())`.
    pub(crate) fn check(&self, n: usize) -> Result<(), Error> {
        let used = self.buf.len() as u64;
        let n = n as u64;
        let new_len = used.checked_add(n).ok_or(Error::LimitExceeded {
            limit: "max_edit_content_bytes",
            max: self.limits.max_edit_content_bytes,
        })?;
        if new_len > self.limits.max_output_bytes {
            return Err(Error::LimitExceeded {
                limit: "max_output_bytes",
                max: self.limits.max_output_bytes,
            });
        }
        let gen_total = self
            .generated_so_far
            .checked_add(new_len)
            .ok_or(Error::LimitExceeded {
                limit: "max_edit_content_bytes",
                max: self.limits.max_edit_content_bytes,
            })?;
        if gen_total > self.limits.max_edit_content_bytes {
            return Err(Error::LimitExceeded {
                limit: "max_edit_content_bytes",
                max: self.limits.max_edit_content_bytes,
            });
        }
        Ok(())
    }

    /// Append one byte — budget checked first.
    pub(crate) fn push(&mut self, b: u8) -> Result<(), Error> {
        self.check(1)?;
        self.buf.push(b);
        Ok(())
    }

    /// Append a slice — budget checked for the *whole* slice first.
    pub(crate) fn extend_from_slice(&mut self, s: &[u8]) -> Result<(), Error> {
        self.check(s.len())?;
        self.buf.extend_from_slice(s);
        Ok(())
    }

    /// Consume the buffer — the caller charges `buf.len()` via
    /// `charge_generated`.
    pub(crate) fn into_inner(self) -> Vec<u8> {
        self.buf
    }
}

/// Serialise one op to content-stream bytes — a small subset of the
/// full grammar, exactly what `remove_text`/`remove_region` emit.
///
/// `generated_so_far` is the cumulative byte count already charged by
/// earlier streams in this call — the writer stops appending once
/// `generated_so_far + buf.len()` would exceed
/// `max_edit_content_bytes`, or `buf.len()` alone would exceed
/// `max_output_bytes`. The check happens *before* every byte lands
/// in the Vec, not after the whole op is written.
///
/// Shared with `remove_image.rs` (M19) and `remove_region.rs` (M20).
pub(crate) fn serialize_ops(
    ops: &[Operation],
    limits: &Limits,
    generated_so_far: u64,
) -> Result<Vec<u8>, Error> {
    let mut out = BoundedWriter::new(limits, generated_so_far);
    let mut ops_written: usize = 0;
    for op in ops {
        ops_written += 1;
        if ops_written > limits.max_operations {
            return Err(Error::LimitExceeded {
                limit: "max_operations",
                max: limits.max_operations as u64,
            });
        }
        write_op(op, &mut out)?;
        out.push(b'\n')?;
    }
    Ok(out.into_inner())
}

/// Charge `stream_len` bytes of generated output against
/// `max_edit_content_bytes` — cumulative across every stream body the
/// call materialises (page `/Contents` + each cloned form/image).
/// Checked arithmetic so a counter can't wrap. Each stream also
/// stays under `serialize_ops`'s `byte_budget` bound, so a single
/// huge body and many small ones are both bounded.
///
/// Shared with `remove_image.rs` (M19) and `remove_region.rs` (M20).
pub(crate) fn charge_generated(acc: u64, stream_len: usize, limits: &Limits) -> Result<u64, Error> {
    let total = acc
        .checked_add(stream_len as u64)
        .ok_or(Error::LimitExceeded {
            limit: "max_edit_content_bytes",
            max: limits.max_edit_content_bytes,
        })?;
    if total > limits.max_edit_content_bytes {
        return Err(Error::LimitExceeded {
            limit: "max_edit_content_bytes",
            max: limits.max_edit_content_bytes,
        });
    }
    Ok(total)
}

/// Serialise a single `Operation` — supports every variant the
/// removal path can emit. Literal-string bytes are written verbatim
/// with `(`/`)`/`\` escaped (the same encoding the parser accepts).
fn write_op(op: &Operation, out: &mut BoundedWriter<'_>) -> Result<(), Error> {
    match op {
        Operation::SaveState => out.extend_from_slice(b"q")?,
        Operation::RestoreState => out.extend_from_slice(b"Q")?,
        Operation::ConcatMatrix(m) => {
            for v in [m.a, m.b, m.c, m.d, m.e, m.f] {
                out.extend_from_slice(fmt_num(v).as_bytes())?;
                out.push(b' ')?;
            }
            out.extend_from_slice(b"cm")?;
        }
        Operation::SetLineWidth(w) => {
            out.extend_from_slice(fmt_num(*w).as_bytes())?;
            out.extend_from_slice(b" w")?;
        }
        Operation::SetLineCap(c) => {
            out.extend_from_slice(c.to_string().as_bytes())?;
            out.extend_from_slice(b" J")?;
        }
        Operation::SetLineJoin(j) => {
            out.extend_from_slice(j.to_string().as_bytes())?;
            out.extend_from_slice(b" j")?;
        }
        Operation::SetMiterLimit(m) => {
            out.extend_from_slice(fmt_num(*m).as_bytes())?;
            out.extend_from_slice(b" M")?;
        }
        Operation::SetDash { dashes, phase } => {
            out.push(b'[')?;
            for (i, v) in dashes.iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(*v).as_bytes())?;
            }
            out.extend_from_slice(b"] ")?;
            out.extend_from_slice(fmt_num(*phase).as_bytes())?;
            out.extend_from_slice(b" d")?;
        }
        Operation::SetRenderingIntent(n) => {
            out.push(b'/')?;
            out.extend_from_slice(n)?;
            out.extend_from_slice(b" ri")?;
        }
        Operation::SetFlatness(i) => {
            out.extend_from_slice(fmt_num(*i).as_bytes())?;
            out.extend_from_slice(b" i")?;
        }
        // Colour ops — pass through unchanged; the operands were
        // validated by the parser (numbers, plus an optional trailing
        // pattern name for SCN/scn — re-emitted as `N` forms when a
        // trailing Name is present).
        Operation::SetGrayStroke(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes())?;
            out.extend_from_slice(b" G")?;
        }
        Operation::SetGrayFill(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes())?;
            out.extend_from_slice(b" g")?;
        }
        Operation::SetRgbStroke(r, g, b) => {
            for (i, v) in [r, g, b].iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(**v).as_bytes())?;
            }
            out.extend_from_slice(b" RG")?;
        }
        Operation::SetRgbFill(r, g, b) => {
            for (i, v) in [r, g, b].iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(**v).as_bytes())?;
            }
            out.extend_from_slice(b" rg")?;
        }
        Operation::SetCmykStroke(c, m, y, k) => {
            for (i, v) in [c, m, y, k].iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(**v).as_bytes())?;
            }
            out.extend_from_slice(b" K")?;
        }
        Operation::SetCmykFill(c, m, y, k) => {
            for (i, v) in [c, m, y, k].iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(**v).as_bytes())?;
            }
            out.extend_from_slice(b" k")?;
        }
        Operation::SetColorSpaceStroke(n) => {
            out.push(b'/')?;
            out.extend_from_slice(n)?;
            out.extend_from_slice(b" CS")?;
        }
        Operation::Type3SetWidth { wx, wy } => {
            out.extend_from_slice(fmt_num(*wx).as_bytes())?;
            out.push(b' ')?;
            out.extend_from_slice(fmt_num(*wy).as_bytes())?;
            out.extend_from_slice(b" d0")?;
        }
        Operation::Type3SetWidthAndBox {
            wx,
            wy,
            llx,
            lly,
            urx,
            ury,
        } => {
            for (i, v) in [wx, wy, llx, lly, urx, ury].iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(**v).as_bytes())?;
            }
            out.extend_from_slice(b" d1")?;
        }
        Operation::SetColorSpaceFill(n) => {
            out.push(b'/')?;
            out.extend_from_slice(n)?;
            out.extend_from_slice(b" cs")?;
        }
        Operation::SetColorStroke(ops) => {
            for o in ops {
                write_object_shallow(o, out)?;
                out.push(b' ')?;
            }
            // `SC` vs `SCN` — the parser merges them; emit `SCN` when
            // a trailing pattern name is present (per §8.6.8).
            if matches!(ops.last(), Some(Object::Name(_))) {
                out.extend_from_slice(b"SCN")?;
            } else {
                out.extend_from_slice(b"SC")?;
            }
        }
        Operation::SetColorFill(ops) => {
            for o in ops {
                write_object_shallow(o, out)?;
                out.push(b' ')?;
            }
            if matches!(ops.last(), Some(Object::Name(_))) {
                out.extend_from_slice(b"scn")?;
            } else {
                out.extend_from_slice(b"sc")?;
            }
        }
        Operation::SetExtGState(n) => {
            out.push(b'/')?;
            out.extend_from_slice(n)?;
            out.extend_from_slice(b" gs")?;
        }
        Operation::MoveTo(x, y) => {
            out.extend_from_slice(fmt_num(*x).as_bytes())?;
            out.push(b' ')?;
            out.extend_from_slice(fmt_num(*y).as_bytes())?;
            out.extend_from_slice(b" m")?;
        }
        Operation::LineTo(x, y) => {
            out.extend_from_slice(fmt_num(*x).as_bytes())?;
            out.push(b' ')?;
            out.extend_from_slice(fmt_num(*y).as_bytes())?;
            out.extend_from_slice(b" l")?;
        }
        Operation::CurveTo(x1, y1, x2, y2, x3, y3) => {
            for (i, v) in [x1, y1, x2, y2, x3, y3].iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(**v).as_bytes())?;
            }
            out.extend_from_slice(b" c")?;
        }
        Operation::CurveToV(x2, y2, x3, y3) => {
            for (i, v) in [x2, y2, x3, y3].iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(**v).as_bytes())?;
            }
            out.extend_from_slice(b" v")?;
        }
        Operation::CurveToY(x1, y1, x3, y3) => {
            for (i, v) in [x1, y1, x3, y3].iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(**v).as_bytes())?;
            }
            out.extend_from_slice(b" y")?;
        }
        Operation::ClosePath => out.extend_from_slice(b"h")?,
        Operation::Rectangle(x, y, w, h) => {
            for (i, v) in [x, y, w, h].iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(**v).as_bytes())?;
            }
            out.extend_from_slice(b" re")?;
        }
        Operation::Stroke => out.extend_from_slice(b"S")?,
        Operation::CloseAndStroke => out.extend_from_slice(b"s")?,
        Operation::Fill => out.extend_from_slice(b"f")?,
        Operation::FillEvenOdd => out.extend_from_slice(b"f*")?,
        Operation::FillAndStroke => out.extend_from_slice(b"B")?,
        Operation::FillAndStrokeEvenOdd => out.extend_from_slice(b"B*")?,
        Operation::CloseFillAndStroke => out.extend_from_slice(b"b")?,
        Operation::CloseFillAndStrokeEvenOdd => out.extend_from_slice(b"b*")?,
        Operation::EndPath => out.extend_from_slice(b"n")?,
        Operation::Clip => out.extend_from_slice(b"W")?,
        Operation::ClipEvenOdd => out.extend_from_slice(b"W*")?,
        Operation::BeginText => out.extend_from_slice(b"BT")?,
        Operation::EndText => out.extend_from_slice(b"ET")?,
        Operation::SetTextFont { font, size } => {
            out.push(b'/')?;
            out.extend_from_slice(font)?;
            out.push(b' ')?;
            out.extend_from_slice(fmt_num(*size).as_bytes())?;
            out.extend_from_slice(b" Tf")?;
        }
        Operation::SetCharSpacing(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes())?;
            out.extend_from_slice(b" Tc")?;
        }
        Operation::SetWordSpacing(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes())?;
            out.extend_from_slice(b" Tw")?;
        }
        Operation::SetHorizontalScaling(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes())?;
            out.extend_from_slice(b" Tz")?;
        }
        Operation::SetTextLeading(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes())?;
            out.extend_from_slice(b" TL")?;
        }
        Operation::SetTextRenderingMode(m) => {
            out.extend_from_slice(m.to_string().as_bytes())?;
            out.extend_from_slice(b" Tr")?;
        }
        Operation::SetTextRise(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes())?;
            out.extend_from_slice(b" Ts")?;
        }
        Operation::MoveTextPosition(x, y) => {
            out.extend_from_slice(fmt_num(*x).as_bytes())?;
            out.push(b' ')?;
            out.extend_from_slice(fmt_num(*y).as_bytes())?;
            out.extend_from_slice(b" Td")?;
        }
        Operation::MoveTextPositionAndSetLeading(x, y) => {
            out.extend_from_slice(fmt_num(*x).as_bytes())?;
            out.push(b' ')?;
            out.extend_from_slice(fmt_num(*y).as_bytes())?;
            out.extend_from_slice(b" TD")?;
        }
        Operation::SetTextMatrix(m) => {
            for v in [m.a, m.b, m.c, m.d, m.e, m.f] {
                out.extend_from_slice(fmt_num(v).as_bytes())?;
                out.push(b' ')?;
            }
            out.extend_from_slice(b"Tm")?;
        }
        Operation::NextLine => out.extend_from_slice(b"T*")?,
        Operation::ShowText(bytes) => {
            write_literal_string(bytes, out)?;
            out.extend_from_slice(b" Tj")?;
        }
        Operation::ShowTextAdjusted(items) => {
            out.push(b'[')?;
            let mut first = true;
            for it in items {
                if !first {
                    out.push(b' ')?;
                }
                first = false;
                match it {
                    TJItem::Text(b) => write_literal_string(b, out)?,
                    TJItem::Kern(n) => out.extend_from_slice(fmt_num(*n).as_bytes())?,
                }
            }
            out.extend_from_slice(b"] TJ")?;
        }
        Operation::NextLineShowText(text) => {
            write_literal_string(text, out)?;
            out.extend_from_slice(b" '")?;
        }
        Operation::SetSpacingNextLineShowText {
            word_spacing,
            char_spacing,
            text,
        } => {
            out.extend_from_slice(fmt_num(*word_spacing).as_bytes())?;
            out.push(b' ')?;
            out.extend_from_slice(fmt_num(*char_spacing).as_bytes())?;
            out.push(b' ')?;
            write_literal_string(text, out)?;
            out.extend_from_slice(b" \"")?;
        }
        Operation::Do(n) => {
            out.push(b'/')?;
            out.extend_from_slice(n)?;
            out.extend_from_slice(b" Do")?;
        }
        Operation::Shading(n) => {
            out.push(b'/')?;
            out.extend_from_slice(n)?;
            out.extend_from_slice(b" sh")?;
        }
        Operation::MarkedContentPoint(n) => {
            out.push(b'/')?;
            out.extend_from_slice(n)?;
            out.extend_from_slice(b" MP")?;
        }
        Operation::DesignatedMarkedContentPoint { tag, properties } => {
            out.push(b'/')?;
            out.extend_from_slice(tag)?;
            out.push(b' ')?;
            write_object_shallow(properties, out)?;
            out.extend_from_slice(b" DP")?;
        }
        Operation::BeginMarkedContent(n) => {
            out.push(b'/')?;
            out.extend_from_slice(n)?;
            out.extend_from_slice(b" BMC")?;
        }
        Operation::BeginMarkedContentWithProps { tag, properties } => {
            out.push(b'/')?;
            out.extend_from_slice(tag)?;
            out.push(b' ')?;
            write_object_shallow(properties, out)?;
            out.extend_from_slice(b" BDC")?;
        }
        Operation::EndMarkedContent => out.extend_from_slice(b"EMC")?,
        Operation::BeginCompatibility => out.extend_from_slice(b"BX")?,
        Operation::EndCompatibility => out.extend_from_slice(b"EX")?,
        // Unknown ops inside BX…EX round-trip verbatim — the operands
        // were parsed objects, re-emitted with the same serialiser.
        // (`Unknown` outside BX…EX never reaches here: the walker's
        // `uncertain_state` taint refuses the page first.)
        Operation::Unknown { operator, operands } => {
            for o in operands {
                write_object_shallow(o, out)?;
                out.push(b' ')?;
            }
            out.extend_from_slice(operator)?;
        }
    }
    Ok(())
}

/// Literal PDF string: `(…)` with `(`/`)`/`\` and control bytes
/// escaped. Non-ASCII bytes are written as `\xNN` octal escapes so
/// the output stays ASCII-clean and re-parses exactly.
fn write_literal_string(bytes: &[u8], out: &mut BoundedWriter<'_>) -> Result<(), Error> {
    out.push(b'(')?;
    for &b in bytes {
        match b {
            b'(' | b')' | b'\\' => {
                out.push(b'\\')?;
                out.push(b)?;
            }
            0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f | 0x7f..=0xff => {
                out.extend_from_slice(format!("\\{:03o}", b).as_bytes())?;
            }
            _ => out.push(b)?,
        }
    }
    out.push(b')')?;
    Ok(())
}

/// Serialise a scalar/array/dict object shallowly — used for
/// marked-content properties and `Unknown` operands. Streams are not
/// representable here (none appear as operands).
fn write_object_shallow(o: &Object, out: &mut BoundedWriter<'_>) -> Result<(), Error> {
    match o {
        Object::Null => out.extend_from_slice(b"null")?,
        Object::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" })?,
        Object::Integer(n) => out.extend_from_slice(n.to_string().as_bytes())?,
        Object::Real(n) => out.extend_from_slice(fmt_num(*n).as_bytes())?,
        Object::Name(n) => {
            out.push(b'/')?;
            out.extend_from_slice(n)?;
        }
        Object::String(s) => write_literal_string(s, out)?,
        Object::Array(a) => {
            out.push(b'[')?;
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                write_object_shallow(x, out)?;
            }
            out.push(b']')?;
        }
        Object::Dict(d) => {
            out.extend_from_slice(b"<<")?;
            for (k, v) in d {
                out.push(b' ')?;
                out.push(b'/')?;
                out.extend_from_slice(k)?;
                out.push(b' ')?;
                write_object_shallow(v, out)?;
            }
            out.extend_from_slice(b" >>")?;
        }
        Object::Ref(r) => {
            out.extend_from_slice(format!("{} {} R", r.number, r.generation).as_bytes())?;
        }
        Object::Stream { .. } => {
            // Unreachable — an operand is never a stream.
            out.extend_from_slice(b"null")?;
        }
    }
    Ok(())
}

/// Format a content-stream number: integer when exact, else fixed-
/// point trimmed to 4 decimals — the same convention `edit.rs` uses
/// for annotation coordinates. `NaN`/`inf` never reach here (every
/// caller validates finite upstream; `write_op` on `ConcatMatrix`
/// re-validates because a `cm` operand could carry them).
fn fmt_num(n: f64) -> String {
    debug_assert!(n.is_finite(), "non-finite operand {n} leaked to writer");
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let s = format!("{n:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reuse the diagnosis builders — same one-glyph TrueType fixture.
    fn one_glyph(body: &[u8]) -> Vec<u8> {
        // Copied minimal: TrueType /F1 + a content stream `body`.
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> >> /Contents 9 0 R >>"
        );
        let widths = {
            let mut w = [600u32; 224];
            w[0] = 250;
            format!(
                "[{}]",
                w.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes()
        };
        let mut font_dict = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        font_dict.extend_from_slice(&widths);
        font_dict.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push!(4, &font_dict);
        push!(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>"
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs.extend_from_slice(&enc);
        fs.extend_from_slice(b"\nendstream");
        push!(6, &fs);
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 10\n0000000000 65535 f \n".to_vec());
        for n in 1..10u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 10 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    fn doc(bytes: &[u8]) -> Document {
        Document::from_bytes(bytes).unwrap()
    }

    /// The 'A' of `(ABC) Tj` at 24pt sits at x=20..34.4 — a rect over
    /// just that region removes exactly the 'A'.
    #[test]
    fn removes_first_char_of_tj() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 20 60 Td (ABC) Tj ET\n"));
        let r = remove_text(&d, 0, &[(20.0, 60.0, 30.0, 80.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // The rewritten stream must be a TJ with (BC) and a numeric
        // element for the removed 'A' advance.
        assert!(out.contains("TJ"), "{out}");
    }

    /// Removing the middle char of `(ABC)` — 'B' sits at 34.4..40.4.
    #[test]
    fn removes_middle_char_of_tj() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 20 60 Td (ABC) Tj ET\n"));
        let r = remove_text(&d, 0, &[(35.0, 60.0, 38.0, 80.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 1);
    }

    /// Removing all three — the TJ still carries the advances so a
    /// hypothetical following glyph would land at the right place.
    #[test]
    fn removes_all_chars_keeps_state() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 20 60 Td (ABC) Tj ET\n"));
        let r = remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 3);
        // The emitted stream should still parse — the `T*`/`Tf`/`Td`
        // state updates survive even though no text is drawn.
        let d2 = doc(&r.output);
        let page = &d2.pages()[0];
        let ops = d2.page_operations(page).unwrap();
        assert!(ops
            .iter()
            .any(|o| matches!(o, Operation::ShowTextAdjusted(_))));
    }

    /// `'` expands to `T*` + `Tj` — the T* must survive an emptied
    /// string.
    #[test]
    fn quote_op_expands_and_keeps_leading() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 20 60 Td 14 TL (A) ' ET\n"));
        let r = remove_text(&d, 0, &[(20.0, 40.0, 30.0, 60.0)], &Limits::default());
        // The ' shows 'A' — its rect overlaps → 1 removed.
        let r = r.unwrap();
        assert_eq!(r.codes_removed, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        assert!(out.contains("T*"), "' must emit T* even when empty: {out}");
    }

    /// A page with /Annots is refused — annotations are out of scope.
    #[test]
    fn annots_page_is_refused() {
        let mut src = one_glyph(b"BT /F1 24 Tf 20 60 Td (A) Tj ET\n");
        // Splice an /Annots entry into the page dict.
        let s = String::from_utf8_lossy(&src).into_owned();
        let s = s.replace(
            "/MediaBox [0 0 200 200]",
            "/MediaBox [0 0 200 200] /Annots [10 0 R]",
        );
        // Need object 10 — a minimal annot. Rebuild with it appended.
        // Easier: the replacement makes the file's xref wrong — build
        // a fresh file with the annot declared.
        let _ = s;
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Annots [10 0 R] /Resources << >> /Contents 4 0 R >>"
        );
        let body = b"BT 20 60 Td ET\n"; // no font needed — nothing shown
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(4, &sb);
        push!(
            10,
            b"<< /Type /Annot /Subtype /Text /Rect [1 1 2 2] /Contents (n) >>"
        );
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        src = parts.concat();
        let d = doc(&src);
        let err = remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default()).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "annots page must refuse, got {err:?}"
        );
    }

    /// A `Do` anywhere on the page is refused — form content is out
    /// of scope.
    #[test]
    fn do_xobject_is_refused() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Fm1 5 0 R >> >> /Contents 4 0 R >>"
        );
        let body = b"/Fm1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(4, &sb);
        push!(
            5,
            b"<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] \
              /Length 0 >>\nstream\n\nendstream"
        );
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 6\n0000000000 65535 f \n".to_vec());
        for n in 1..6u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            }
        }
        parts.push(b"trailer\n<< /Size 6 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // M18: a Form `Do` is in scope — an empty form with no
        // removals succeeds (no clone is needed since nothing was
        // selected inside it).
        let r = remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 0);
    }

    /// A 1-page PDF with a Form XObject invoked once — `/Fm1` at
    /// `10 0 R`. `form_body` runs inside the form under the page's
    /// `/F1` (inherited resources: the form has no `/Resources` of its
    /// own so it uses the caller's, §8.10.1).
    fn form_pdf(form_body: &[u8]) -> Vec<u8> {
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> /XObject << /Fm1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let widths = {
            let mut w = [600u32; 224];
            w[0] = 250;
            format!(
                "[{}]",
                w.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes()
        };
        let mut fd = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        fd.extend_from_slice(&widths);
        fd.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push!(4, &fd);
        push!(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>"
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs2 = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs2.extend_from_slice(&enc);
        fs2.extend_from_slice(b"\nendstream");
        push!(6, &fs2);
        // Page content: one `Do` at op #0.
        let body = b"/Fm1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        // Form at 10 0 R — identity /Matrix, 200×200 /BBox, no own
        // /Resources (inherits the page's).
        let mut f = b"<< /Type /XObject /Subtype /Form /BBox [0 0 200 200] \
              /Matrix [1 0 0 1 0 0] /Length "
            .to_vec();
        f.extend_from_slice(form_body.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(form_body);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    /// A page that invokes `/Fm1` `invocations` times at identical
    /// CTM — used to exercise cumulative budgets across repeated
    /// calls of the same small form.
    fn form_n_invocations_pdf(form_body: &[u8], invocations: usize) -> Vec<u8> {
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> /XObject << /Fm1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let widths = {
            let mut w = [600u32; 224];
            w[0] = 250;
            format!(
                "[{}]",
                w.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes()
        };
        let mut fd = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        fd.extend_from_slice(&widths);
        fd.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push!(4, &fd);
        push!(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>"
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs2 = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs2.extend_from_slice(&enc);
        fs2.extend_from_slice(b"\nendstream");
        push!(6, &fs2);
        let mut body = Vec::new();
        for _ in 0..invocations {
            body.extend_from_slice(b"/Fm1 Do\n");
        }
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(&body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let mut f = b"<< /Type /XObject /Subtype /Form /BBox [0 0 200 200] \
              /Matrix [1 0 0 1 0 0] /Length "
            .to_vec();
        f.extend_from_slice(form_body.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(form_body);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    /// M18 — remove a code inside a Form `Do`: the invocation is
    /// cloned (new `/XObject` name + new stream object), the page's
    /// `Do` points at the clone, and the *original* form object is
    /// untouched in the output.
    #[test]
    fn form_text_removal_clones_the_invocation() {
        // Form paints (ABC) at 24pt under /F1 — 'B' sits at
        // origin 34.4..40.4, ink ~34.4..40.4, y 60..76.8.
        let d = doc(&form_pdf(b"BT /F1 24 Tf 20 60 Td (ABC) Tj ET\n"));
        let r = remove_text(&d, 0, &[(35.0, 60.0, 38.0, 80.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // The clone's stream must carry the same rewrite a page-level
        // removal would: [(A) -600 (C)] TJ at op-time Tc=0, w0=600.
        assert!(
            out.contains("[(A) -600 (C)] TJ"),
            "cloned form must rewrite the Tj: {out}"
        );
        // The original form object (10 0 R) must still contain the
        // *unmodified* string — its bytes are preserved verbatim.
        assert!(
            out.contains("(ABC)"),
            "original form must keep its (ABC) string: {out}"
        );
        // A new resource name was registered and a `Do` rewritten.
        assert!(
            out.contains("/KasFm"),
            "cloned form must get a fresh /XObject name: {out}"
        );
    }

    /// M18 — two `Do`s of the *same* form are distinct invocations:
    /// only the one whose inner text a rect hits is cloned; the other
    /// keeps pointing at the original object.
    #[test]
    fn two_invocations_only_the_hit_one_is_cloned() {
        // /Fm1 twice at different translations — 'B' inside the
        // first invocation's copy at (20,60)+(34.4,0) ≈ origin
        // 54.4..60.4; the second copy is at (120,60).
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> /XObject << /Fm1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let widths = {
            let mut w = [600u32; 224];
            w[0] = 250;
            format!(
                "[{}]",
                w.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes()
        };
        let mut fd = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        fd.extend_from_slice(&widths);
        fd.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push!(4, &fd);
        push!(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>"
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs2 = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs2.extend_from_slice(&enc);
        fs2.extend_from_slice(b"\nendstream");
        push!(6, &fs2);
        // Two `Do`s of the same form at translations (20,60) and
        // (120,60) — op #2 and op #5 (cm + Do each).
        let body = b"q 1 0 0 1 20 60 cm /Fm1 Do Q\nq 1 0 0 1 120 60 cm /Fm1 Do Q\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let form_body = b"BT /F1 24 Tf 0 0 Td (ABC) Tj ET\n";
        let mut f = b"<< /Type /XObject /Subtype /Form /BBox [0 0 200 200] \
              /Matrix [1 0 0 1 0 0] /Length "
            .to_vec();
        f.extend_from_slice(form_body.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(form_body);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // 'B' inside invocation 1: ink ~34.4..40.4 — rect edges stay
        // strictly inside so A's `20..34.4` and C's `57.2..63.2`
        // (edge-touching counts as a hit) are both missed.
        // Invocation 2's 'B' is at 120+34.4=154.4 — outside.
        let r = remove_text(&d, 0, &[(34.5, 55.0, 40.0, 80.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // The clone carries the rewrite; the original form keeps
        // "(ABC)" so invocation 2 still shows all three glyphs.
        assert!(out.contains("[(A) -600 (C)] TJ"), "clone: {out}");
        assert!(out.contains("(ABC)"), "original keeps (ABC): {out}");
        // Exactly one `Do` was renamed — the other still says `/Fm1`.
        let fm1_count = out.matches("/Fm1 Do").count();
        let kasm_do = out.matches("/KasFm0 Do").count();
        assert_eq!(fm1_count, 1, "one /Fm1 Do remains: {out}");
        assert_eq!(kasm_do, 1, "one /KasFm0 Do appeared: {out}");
    }

    /// M18 — the caller's text state is inherited by the form: `Tc`
    /// set at page level before the `Do` applies to the form's `Tj`,
    /// so the kern carries `Tc` even though the form never sets it.
    #[test]
    fn form_inherits_caller_text_state() {
        // `2 Tc` at page level (text state is graphics state) — the
        // form's 'B' advance = 14.4+2 = 16.4 → origin 36.4, ink
        // ~36.4..42.4 (rect 36..43 hits only it).
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> /XObject << /Fm1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let widths = {
            let mut w = [600u32; 224];
            w[0] = 250;
            format!(
                "[{}]",
                w.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes()
        };
        let mut fd = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        fd.extend_from_slice(&widths);
        fd.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push!(4, &fd);
        push!(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>"
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs2 = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs2.extend_from_slice(&enc);
        fs2.extend_from_slice(b"\nendstream");
        push!(6, &fs2);
        let body = b"2 Tc\nq 1 0 0 1 20 60 cm /Fm1 Do Q\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let form_body = b"BT /F1 24 Tf 0 0 Td (ABC) Tj ET\n";
        let mut f = b"<< /Type /XObject /Subtype /Form /BBox [0 0 200 200] \
              /Matrix [1 0 0 1 0 0] /Length "
            .to_vec();
        f.extend_from_slice(form_body.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(form_body);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let r = remove_text(&d, 0, &[(36.0, 55.0, 43.0, 80.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // Kern at *inherited* Tc=2: -(600 + 2·1000/24) = -683.3333 —
        // a snapshot taken inside the form sees the caller's Tc.
        assert!(out.contains("-683.3333"), "inherited Tc=2 kern: {out}");
    }

    /// M18 — rect hits at *both* levels: page-level text is rewritten
    /// in the page stream while the form invocation is cloned — the
    // two scopes compose.
    #[test]
    fn form_and_page_text_both_rewrite() {
        // Page shows (XYZ) at (20,120); form shows (ABC) at (20,60).
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> /XObject << /Fm1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let widths = {
            let mut w = [600u32; 224];
            w[0] = 250;
            format!(
                "[{}]",
                w.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes()
        };
        let mut fd = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        fd.extend_from_slice(&widths);
        fd.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push!(4, &fd);
        push!(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>"
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs2 = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs2.extend_from_slice(&enc);
        fs2.extend_from_slice(b"\nendstream");
        push!(6, &fs2);
        let body = b"q 1 0 0 1 20 60 cm /Fm1 Do Q\nBT /F1 24 Tf 20 120 Td (XYZ) Tj ET\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let form_body = b"BT /F1 24 Tf 0 0 Td (ABC) Tj ET\n";
        let mut f = b"<< /Type /XObject /Subtype /Form /BBox [0 0 200 200] \
              /Matrix [1 0 0 1 0 0] /Length "
            .to_vec();
        f.extend_from_slice(form_body.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(form_body);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // 'Y' on the page (ink 34.4..40.4 @y120) + 'B' in the form
        // (ink 34.4..40.4 @y60 — form cm(20,60) + inner Td(0,0)).
        let r = remove_text(
            &d,
            0,
            &[(34.5, 115.0, 40.0, 140.0), (34.5, 55.0, 40.0, 80.0)],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.codes_removed, 2);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // Page Tj: [(X) -600 (Z)] — form clone: [(A) -600 (C)].
        assert!(out.contains("[(X) -600 (Z)] TJ"), "page rewrite: {out}");
        assert!(out.contains("[(A) -600 (C)] TJ"), "form clone: {out}");
    }

    /// M18 — a form stream that *is* `/Filter`ed is decoded, rewritten,
    /// and emitted uncompressed — the clone dict must not keep the
    /// stale `/Filter` (the decoder would otherwise re-apply it).
    #[test]
    fn filtered_form_clone_strips_filter() {
        // Form body stored Flate-compressed; removal must clone it as
        // a plain stream without `/Filter`/`/DecodeParms`.
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> /XObject << /Fm1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let widths = {
            let mut w = [600u32; 224];
            w[0] = 250;
            format!(
                "[{}]",
                w.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes()
        };
        let mut fd = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        fd.extend_from_slice(&widths);
        fd.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push!(4, &fd);
        push!(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>"
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs2 = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs2.extend_from_slice(&enc);
        fs2.extend_from_slice(b"\nendstream");
        push!(6, &fs2);
        push!(9, &{
            let body = b"/Fm1 Do\n";
            let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
            sb.extend_from_slice(body);
            sb.extend_from_slice(b"\nendstream");
            sb
        });
        // Form stored compressed.
        let form_body = b"BT /F1 24 Tf 0 0 Td (ABC) Tj ET\n";
        let fenc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(form_body).unwrap();
            e.finish().unwrap()
        };
        let mut f = format!(
            "<< /Type /XObject /Subtype /Form /BBox [0 0 200 200] \
              /Matrix [1 0 0 1 0 0] /Filter /FlateDecode /Length {} >>\nstream\n",
            fenc.len()
        )
        .into_bytes();
        f.extend_from_slice(&fenc);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // 'B' ink 14.4..20.4 — edges strictly inside so A (0..14.4)
        // and C (37.2..43.2) are missed.
        let r = remove_text(&d, 0, &[(14.5, 0.0, 20.0, 20.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // The clone carries the rewrite uncompressed — no `/Filter`
        // near its stream header.
        assert!(out.contains("[(A) -600 (C)] TJ"), "clone rewrite: {out}");
        // The *clone* carries the rewrite uncompressed — its dict
        // must not declare `/Filter`. Find the obj whose stream
        // contains the TJ (the `/XObject` entry `/KasFm0 N 0 R`
        // points at it; object order in the output isn't fixed).
        let tj_pos = out.find("[(A) -600 (C)] TJ").expect("clone body");
        let dict_start = out[..tj_pos].rfind("<< /BBox").expect("clone dict");
        let dict_text = &out[dict_start..tj_pos];
        assert!(
            !dict_text.contains("/Filter"),
            "clone must not carry the old /Filter: {dict_text}"
        );
        // The *original* still carries its `/Filter` + encoded bytes —
        // assert it's untouched by finding `/FlateDecode` surviving.
        assert!(
            out.contains("/FlateDecode"),
            "original keeps /Filter: {out}"
        );
    }

    /// M18 — a form carrying `/Group` (transparency group) is refused:
    /// the walker already blocks on it, and the refusal survives to
    /// remove_text even when the rect selects its text.
    #[test]
    fn form_with_group_is_refused() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> /XObject << /Fm1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let widths = {
            let mut w = [600u32; 224];
            w[0] = 250;
            format!(
                "[{}]",
                w.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes()
        };
        let mut fd = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        fd.extend_from_slice(&widths);
        fd.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push!(4, &fd);
        push!(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>"
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs2 = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs2.extend_from_slice(&enc);
        fs2.extend_from_slice(b"\nendstream");
        push!(6, &fs2);
        push!(9, &{
            let body = b"/Fm1 Do\n";
            let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
            sb.extend_from_slice(body);
            sb.extend_from_slice(b"\nendstream");
            sb
        });
        let form_body = b"BT /F1 24 Tf 0 0 Td (ABC) Tj ET\n";
        let mut f = b"<< /Type /XObject /Subtype /Form /BBox [0 0 200 200] \
              /Matrix [1 0 0 1 0 0] /Group << /S /Transparency >> /Length "
            .to_vec();
        f.extend_from_slice(form_body.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(form_body);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let err = remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default()).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "/Group must refuse: {err:?}"
        );
    }

    /// M18 — the same form object invoked on *two pages*: editing
    /// page 1's invocation leaves page 2's `Do` (and the shared form)
    /// untouched — only page 1's resources gain the clone.
    #[test]
    fn shared_form_other_page_keeps_original() {
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R 8 0 R] /Count 2 >>");
        // Two pages — each calls /Fm1 at identity CTM, sharing 10 0 R.
        for (page, contents) in [(3u32, 9u32), (8u32, 12u32)] {
            let body = b"/Fm1 Do\n";
            let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
            sb.extend_from_slice(body);
            sb.extend_from_slice(b"\nendstream");
            push!(contents, &sb);
            let pdict = format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                 /Resources << /Font << /F1 4 0 R >> /XObject << /Fm1 10 0 R >> >> \
                 /Contents {contents} 0 R >>"
            )
            .into_bytes();
            push!(page, &pdict);
        }
        let widths = {
            let mut w = [600u32; 224];
            w[0] = 250;
            format!(
                "[{}]",
                w.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes()
        };
        let mut fd = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        fd.extend_from_slice(&widths);
        fd.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push!(4, &fd);
        push!(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>"
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs2 = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs2.extend_from_slice(&enc);
        fs2.extend_from_slice(b"\nendstream");
        push!(6, &fs2);
        let form_body = b"BT /F1 24 Tf 0 0 Td (ABC) Tj ET\n";
        let mut f = b"<< /Type /XObject /Subtype /Form /BBox [0 0 200 200] \
              /Matrix [1 0 0 1 0 0] /Length "
            .to_vec();
        f.extend_from_slice(form_body.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(form_body);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 13\n0000000000 65535 f \n".to_vec());
        for n in 1..13u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 13 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // Remove 'B' on page 1 only — page 2's invocation shares the
        // same form object and must keep all three glyphs.
        let r = remove_text(&d, 0, &[(14.5, 0.0, 20.0, 20.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // The original form keeps `(ABC)` — page 2 still shows it.
        assert!(out.contains("(ABC)"), "shared original intact: {out}");
        assert!(
            out.contains("[(A) -600 (C)] TJ"),
            "page 1 clone rewrote: {out}"
        );
        // Two `/Fm1 Do` calls would mean both pages renamed — exactly
        // one `/Fm1 Do` must survive (page 2's), one `/KasFm0 Do`
        // appears (page 1's).
        assert_eq!(
            out.matches("/Fm1 Do").count(),
            1,
            "page 2 keeps /Fm1: {out}"
        );
        assert_eq!(out.matches("/KasFm0 Do").count(), 1, "page 1 cloned: {out}");
    }

    /// M18 — a `Do` inside a form refuses even when the nested form
    /// is *empty* (no inner text to trip the depth check): the
    /// invocation record itself (`Form` target at depth 1) is the
    /// nested signal.
    #[test]
    fn empty_nested_do_is_refused() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> /XObject << /Outer 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let widths = {
            let mut w = [600u32; 224];
            w[0] = 250;
            format!(
                "[{}]",
                w.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes()
        };
        let mut fd = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        fd.extend_from_slice(&widths);
        fd.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push!(4, &fd);
        push!(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>"
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs2 = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs2.extend_from_slice(&enc);
        fs2.extend_from_slice(b"\nendstream");
        push!(6, &fs2);
        push!(9, &{
            let body = b"/Outer Do\n";
            let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
            sb.extend_from_slice(body);
            sb.extend_from_slice(b"\nendstream");
            sb
        });
        let outer_body = b"/Inner Do\n";
        let mut f = b"<< /Type /XObject /Subtype /Form /BBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> /XObject << /Inner 11 0 R >> >> \
              /Length "
            .to_vec();
        f.extend_from_slice(outer_body.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(outer_body);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        // Inner form is EMPTY — no inner text targets.
        let f2 = b"<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] \
              /Length 0 >>\nstream\n\nendstream"
            .to_vec();
        push!(11, &f2);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 12\n0000000000 65535 f \n".to_vec());
        for n in 1..12u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 12 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let err = remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default()).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "empty nested Do must refuse: {err:?}"
        );
    }

    /// M18 — a `Do` *inside* a form refuses (nested XObjects).
    #[test]
    fn nested_do_is_refused() {
        // The outer form contains a `Do` of the inner form.
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> /XObject << /Outer 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let widths = {
            let mut w = [600u32; 224];
            w[0] = 250;
            format!(
                "[{}]",
                w.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes()
        };
        let mut fd = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        fd.extend_from_slice(&widths);
        fd.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push!(4, &fd);
        push!(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>"
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs2 = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs2.extend_from_slice(&enc);
        fs2.extend_from_slice(b"\nendstream");
        push!(6, &fs2);
        push!(9, &{
            let body = b"/Outer Do\n";
            let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
            sb.extend_from_slice(body);
            sb.extend_from_slice(b"\nendstream");
            sb
        });
        // Outer form — its own /Resources must carry the /Font the
        // inner form needs (a form's /Resources *replaces* the
        // caller's, §8.10.1 — it does not merge).
        let outer_body = b"/Inner Do\n";
        let mut f = b"<< /Type /XObject /Subtype /Form /BBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> /XObject << /Inner 11 0 R >> >> \
              /Length "
            .to_vec();
        f.extend_from_slice(outer_body.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(outer_body);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let inner_body = b"BT /F1 24 Tf 20 60 Td (ABC) Tj ET\n";
        let mut f2 = b"<< /Type /XObject /Subtype /Form /BBox [0 0 200 200] \
              /Length "
            .to_vec();
        f2.extend_from_slice(inner_body.len().to_string().as_bytes());
        f2.extend_from_slice(b" >>\nstream\n");
        f2.extend_from_slice(inner_body);
        f2.extend_from_slice(b"\nendstream");
        push!(11, &f2);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 12\n0000000000 65535 f \n".to_vec());
        for n in 1..12u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 12 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let err = remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default()).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "nested Do must refuse: {err:?}"
        );
    }

    /// An *image* `Do` is still refused — form text is the only
    /// XObject kind M18 edits.
    #[test]
    fn image_xobject_is_refused() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> /XObject << /Im1 6 0 R >> >> \
              /Contents 9 0 R >>"
        );
        // Reuse the one-glyph fixture for the font — the image is a
        // 1x1 DeviceGray stream.
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let widths = {
            let mut w = [600u32; 224];
            w[0] = 250;
            format!(
                "[{}]",
                w.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes()
        };
        let mut fd = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        fd.extend_from_slice(&widths);
        fd.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push!(4, &fd);
        push!(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>"
        );
        push!(
            6,
            b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\nstream\n\x80\nendstream"
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs2 = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs2.extend_from_slice(&enc);
        fs2.extend_from_slice(b"\nendstream");
        push!(7, &fs2);
        let body = b"BT /F1 24 Tf 20 60 Td (A) Tj ET\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 10\n0000000000 65535 f \n".to_vec());
        for n in 1..10u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 10 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let err = remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default()).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "image Do must refuse, got {err:?}"
        );
    }

    /// The output is a valid single-revision PDF — re-parse it and
    /// the removed text is gone from extraction.
    #[test]
    fn output_reparse_removes_char() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 20 60 Td (ABC) Tj ET\n"));
        let r = remove_text(&d, 0, &[(35.0, 60.0, 38.0, 80.0)], &Limits::default()).unwrap();
        let d2 = doc(&r.output);
        let text = crate::extract_text(&d2, &d2.pages()[0], &Limits::default()).unwrap();
        let s: String = text
            .chars
            .iter()
            .filter_map(|c| c.unicode.as_deref())
            .collect();
        assert_eq!(s, "AC", "B removed: {s:?}");
    }

    /// Removing 'B' from `(ABC) Tj` produces `[(A) <-600> (C)] TJ`
    /// — the kern lands between the kept runs so 'C' keeps its origin.
    /// `w0` for 'B' is 600/1000em → 600 as a TJ number (Tc=Tw=0, Tz=1).
    #[test]
    fn tj_output_order_kern_between_strings() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 20 60 Td (ABC) Tj ET\n"));
        let r = remove_text(&d, 0, &[(35.0, 60.0, 38.0, 80.0)], &Limits::default()).unwrap();
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // Extract the new Contents stream — it must be [(A) -600 (C)]
        // TJ so 'C' lands at x=48.8, not x=34.4.
        assert!(
            out.contains("[(A) -600 (C)] TJ"),
            "kern must sit between kept strings: {out}"
        );
    }

    /// Removing the last char of `(ABC) Tj` — a trailing kern still
    /// preserves the advance for any following op's first glyph.
    #[test]
    fn trailing_removal_emits_trailing_kern() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 20 60 Td (ABC) Tj ET\n"));
        let r = remove_text(&d, 0, &[(48.0, 60.0, 63.0, 80.0)], &Limits::default()).unwrap();
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // C's kern follows the kept (AB).
        assert!(
            out.contains("[(AB) -600] TJ"),
            "trailing kern must preserve C's advance: {out}"
        );
    }

    /// Removing the first char of `(ABC) Tj` — kern precedes the
    /// surviving `(BC)`.
    #[test]
    fn leading_removal_emits_leading_kern() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 20 60 Td (ABC) Tj ET\n"));
        let r = remove_text(&d, 0, &[(20.0, 60.0, 30.0, 80.0)], &Limits::default()).unwrap();
        let out = String::from_utf8_lossy(&r.output).into_owned();
        assert!(
            out.contains("[-600 (BC)] TJ"),
            "leading kern must preserve A's advance: {out}"
        );
    }

    /// `TJ` — numeric elements pass through; removed codes split the
    /// surrounding string.
    #[test]
    fn tj_items_preserve_numeric_elements() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 20 60 Td [(A) 100 (BC)] TJ ET\n"));
        // Remove B only (rect covers its bounds).
        let r = remove_text(&d, 0, &[(35.0, 60.0, 38.0, 80.0)], &Limits::default()).unwrap();
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // Original [(A) 100 (BC)] → 'B' removed →
        // [(A) 100 -600 (C)] — B's advance replaces its string.
        assert!(
            out.contains("[(A) 100 -600 (C)] TJ"),
            "TJ must keep the original 100 and add B's kern: {out}"
        );
    }

    /// A removal whose advance straddles a numeric element —
    /// `[(AB) 50 (C)]` removing 'B' must place B's kern *before* the
    /// existing `50` so 'C' keeps its origin.
    #[test]
    fn tj_removal_advance_crosses_numeric_element() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 20 60 Td [(AB) 50 (C)] TJ ET\n"));
        let r = remove_text(&d, 0, &[(35.0, 60.0, 38.0, 80.0)], &Limits::default()).unwrap();
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // (A) then B's advance -600, then the original 50 kern, then (C).
        assert!(
            out.contains("[(A) -600 50 (C)] TJ"),
            "removed advance must precede the original kern: {out}"
        );
    }

    /// `'` (NextLineShowText) — expands to `T*` + `TJ`; the T* must
    /// survive even when the text is fully removed.
    #[test]
    fn quote_expands_and_t_star_survives() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 14 TL 20 60 Td (A) ' (B) ' ET\n"));
        // Remove the 'A' — rect covers only its bounds (y=60..76.8).
        let r = remove_text(&d, 0, &[(15.0, 55.0, 30.0, 80.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // The `'` became T* + [-600] TJ — the line move survives an
        // emptied string.
        assert!(
            out.contains("T*\n[-600] TJ"),
            "' must expand to T* + TJ with kern: {out}"
        );
    }

    /// `"` (SetSpacingNextLineShowText) — expands to `Tw` + `Tc` +
    /// `T*` + `TJ`; all side effects survive.
    #[test]
    fn double_quote_expands_all_side_effects() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 20 60 Td 5 2 (A) \" (B) Tj ET\n"));
        let r = remove_text(&d, 0, &[(15.0, 55.0, 30.0, 80.0)], &Limits::default()).unwrap();
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // " → Tw Tc T* + TJ — every state change survives.
        assert!(
            out.contains("5 Tw\n2 Tc\nT*\n"),
            "\" must emit Tw + Tc + T*: {out}"
        );
    }

    /// A stroke-mode (Tr 1) text page is refused — only Tr 0 is in
    /// scope.
    #[test]
    fn stroke_mode_text_is_refused() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 20 60 Td 1 Tr (ABC) Tj ET\n"));
        let err = remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default()).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
    }

    /// M17.1 — an invisible (Tr 3) text occurrence *outside* the
    /// removal rect is still out of scope: the page's eligibility is
    /// decided by what it *shows*, not by what was selected. The
    /// current code only checks the render mode of *hit* targets, so
    /// out-of-rect Tr 3 text slips through — fix that.
    #[test]
    fn invisible_text_outside_rect_is_refused() {
        // (A) at Tr 0 on line 1; (B) at Tr 3 on line 2 — only 'A' is
        // inside the rect. The Tr 3 occurrence is out of scope even
        // though nothing removes it.
        let d = doc(&one_glyph(
            b"BT /F1 24 Tf 0 Tr 20 100 Td (A) Tj 3 Tr 20 60 Td (B) Tj ET\n",
        ));
        let err = remove_text(&d, 0, &[(15.0, 95.0, 40.0, 125.0)], &Limits::default()).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
    }

    /// Same for stroke modes — Tr 1 text far from the rect refuses
    /// the page, not just refuses-to-remove.
    #[test]
    fn stroke_mode_outside_rect_is_refused() {
        let d = doc(&one_glyph(
            b"BT /F1 24 Tf 0 Tr 20 100 Td (A) Tj 1 Tr 20 60 Td (B) Tj ET\n",
        ));
        let err = remove_text(&d, 0, &[(15.0, 95.0, 40.0, 125.0)], &Limits::default()).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
    }

    /// A page whose *only* text is Tr 3 — nothing is even selected —
    /// is still refused (the unsupported occurrence exists).
    #[test]
    fn page_with_only_invisible_text_is_refused() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 3 Tr 20 60 Td (B) Tj ET\n"));
        let err =
            remove_text(&d, 0, &[(150.0, 150.0, 190.0, 190.0)], &Limits::default()).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
    }

    /// Tr set to an unsupported mode but *never used* to show text is
    /// fine — the mode only matters when glyphs are shown under it.
    /// `3 Tr` then `0 Tr` before any `Tj` shows nothing unsupported.
    #[test]
    fn unused_render_mode_is_allowed() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 3 Tr 0 Tr 20 60 Td (AB) Tj ET\n"));
        let r = remove_text(&d, 0, &[(35.0, 60.0, 38.0, 80.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 1);
    }

    /// An invisible (Tr 3) text page is refused — invisible text is
    /// not evaluated.
    #[test]
    fn invisible_text_is_refused() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 20 60 Td 3 Tr (ABC) Tj ET\n"));
        let err = remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default()).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
    }

    /// A 1-page PDF with *two* TrueType fonts — `/F1` with all
    /// widths 600 and `/F2` with all widths 300 — so a code shown
    /// under `/F2` advances half as far. The same font file backs
    /// both; only the `/Widths` differ.
    fn two_fonts(body: &[u8]) -> Vec<u8> {
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R /F2 7 0 R >> >> /Contents 9 0 R >>"
        );
        let mk_font = |w: u32| -> Vec<u8> {
            let widths = {
                let wv = [w; 224];
                format!(
                    "[{}]",
                    wv.iter()
                        .map(|v| v.to_string())
                        .collect::<Vec<_>>()
                        .join(" ")
                )
                .into_bytes()
            };
            let mut d = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
                  /FirstChar 32 /LastChar 255 /Widths "
                .to_vec();
            d.extend_from_slice(&widths);
            d.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
            d
        };
        push!(4, &mk_font(600));
        push!(7, &mk_font(300));
        push!(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>"
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs.extend_from_slice(&enc);
        fs.extend_from_slice(b"\nendstream");
        push!(6, &fs);
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 10\n0000000000 65535 f \n".to_vec());
        for n in 1..10u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 10 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    /// M17.1 — a font switch between shows must not leak the second
    /// font's widths into the first op's kern. `/F1` widths are 600,
    /// `/F2` 300: removing 'B' under `/F1` (k=600) and 'C' under
    /// `/F2` (k=300) must each carry their own width.
    #[test]
    fn font_switch_uses_per_op_widths() {
        // Two `Tj`s, one per font, on the same line — `(AB)` under
        // /F1 (w 600) then `(C)` under /F2 (w 300). 'B' sits at
        // origin 34.4..40.4; 'C' follows at origin 48.8 with ink
        // 48.8..54.8 (w 300 advance 7.2 → ink 48.8..55.2 approx).
        let d = doc(&two_fonts(
            b"BT /F1 24 Tf 20 60 Td (AB) Tj /F2 24 Tf (C) Tj ET\n",
        ));
        // Remove 'B' (ink 34.4..40.4) and 'C' (ink ~48.8..55.2 —
        // verify the actual bounds rather than guess).
        let r = remove_text(
            &d,
            0,
            &[(34.5, 55.0, 42.0, 80.0), (47.0, 55.0, 60.0, 80.0)],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.codes_removed, 2);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // 'B' kern under /F1: -(600 + 0·1000/24) = -600.
        // 'C' kern under /F2: -(300 + 0·1000/24) = -300.
        // The buggy end-of-stream font (/F2, w 300) would emit -300
        // for 'B' too.
        assert!(out.contains("[(A) -600] TJ"), "F1 kern 600: {out}");
        assert!(out.contains("[-300] TJ"), "F2 kern 300: {out}");
    }

    /// M17.1 — `q`/`Q` restores text state (Tc/Tw/Tz are graphics
    /// state): a `Tc` set inside `q…Q` must not leak past the `Q`,
    /// and the kern for a glyph shown *after* `Q` uses the restored
    /// value.
    #[test]
    fn q_restore_uses_restored_state() {
        // `Tc=7` inside q…Q — `Tf` is *also* inside the q so the
        // second BT re-selects the font after the Q restore.
        let d = doc(&one_glyph(
            b"q 7 Tc BT /F1 24 Tf 20 60 Td (AB) Tj ET Q\nBT /F1 24 Tf 20 120 Td (C) Tj ET\n",
        ));
        // 'B' advance at Tc=7: (14.4+7)=21.4 → origin 41.4, ink ~41.4..47.4.
        // 'C' on line 2: the second BT resets Tm — origin x=20,
        // ink 20..34.4 at y 120.
        let r = remove_text(
            &d,
            0,
            &[(40.0, 55.0, 50.0, 80.0), (15.0, 115.0, 40.0, 140.0)],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.codes_removed, 2);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // 'B' kern at Tc=7: -(600 + 7·1000/24) = -891.6667.
        // 'C' kern at restored Tc=0: -(600 + 0) = -600.
        assert!(out.contains("[(A) -891.6667] TJ"), "Tc=7 kern: {out}");
        assert!(out.contains("[-600] TJ"), "restored Tc=0 kern: {out}");
    }

    /// M17.1 — trailing state ops (`Tf`/`Tc` with no following show)
    /// must not change an earlier op's rewrite.
    #[test]
    fn trailing_state_ops_do_not_change_rewrite() {
        let d = doc(&one_glyph(
            b"BT /F1 24 Tf 2 Tc 20 60 Td (AB) Tj /F1 10 Tf 9 Tc 50 Tz ET\n",
        ));
        // 'B' advance at Tc=2: 16.4 → origin 36.4, ink ~36.4..42.4.
        let r = remove_text(&d, 0, &[(35.0, 55.0, 45.0, 80.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // Kern at Tc=2: -(600 + 2·1000/24) = -683.3333 — the trailing
        // `9 Tc`/`50 Tz`/`10 Tf` must not alter it.
        assert!(out.contains("[(A) -683.3333] TJ"), "op-time state: {out}");
    }

    /// M17.1 — multiple `BT`/`ET`: each `Tj` uses the state inside
    /// its own text object.
    #[test]
    fn multiple_bt_et_each_use_own_state() {
        let d = doc(&one_glyph(
            b"BT /F1 24 Tf 4 Tc 20 60 Td (AB) Tj ET\nBT 8 Tc 20 120 Td (CD) Tj ET\n",
        ));
        // 'B' Tc=4: advance 18.4, origin 38.4, ink 38.4..44.4 y60.
        // 'C' Tc=8 line2: origin x=20, ink 20..34.4 y120.
        let r = remove_text(
            &d,
            0,
            &[(37.0, 55.0, 46.0, 80.0), (15.0, 115.0, 40.0, 140.0)],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.codes_removed, 2);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // 'B': -(600 + 4·1000/24) = -766.6667. 'C': -(600 + 8·1000/24) = -933.3333.
        assert!(out.contains("[(A) -766.6667] TJ"), "BT1 Tc=4: {out}");
        assert!(out.contains("[-933.3333 (D)] TJ"), "BT2 Tc=8: {out}");
    }

    /// M17.1 — `'` runs under the *current* text state like any
    /// show: a `Tc` set before it applies to its kern too.
    #[test]
    fn quote_uses_op_state() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 6 Tc 14 TL 20 60 Td (AB) ' ET\n"));
        // 'A' advance Tc=6: 20.4 → 'B' origin 40.4, ink 40.4..46.4.
        // The `'` also emits `T*` first (line down by TL=14).
        let r = remove_text(&d, 0, &[(39.0, 50.0, 48.0, 62.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // `'` → T* + [(A) <-(600 + 6·1000/24)>] TJ = [(A) -850] TJ.
        assert!(
            out.contains("T*\n[(A) -850] TJ"),
            "' kern must use the op's Tc=6: {out}"
        );
    }

    /// M17.1 — `"` updates Tw/Tc *before* showing: the snapshot
    /// carries the op's new values so the kern math matches.
    #[test]
    fn double_quote_uses_new_spacing() {
        let d = doc(&one_glyph(
            b"BT /F1 24 Tf 14 TL 20 60 Td (A) Tj 4 2 (B) \" ET\n",
        ));
        // `"` sets Tw=4 Tc=2, moves a line, shows (B) — 'B' origin
        // x=20 at y 60-14=46, ink 20..34.4. Remove 'B'.
        let r = remove_text(&d, 0, &[(15.0, 40.0, 36.0, 55.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // `"` → Tw + Tc + T* + [-683.3333] TJ where
        // -(600 + 2·1000/24) = -683.3333 (Tw applies to spaces only).
        assert!(
            out.contains("4 Tw\n2 Tc\nT*\n[-683.3333] TJ"),
            "\" kern must use its own Tc=2: {out}"
        );
    }

    /// A 1-page PDF with `/F1` simple TrueType (1-byte codes, w 600)
    /// and `/F2` Type0/Identity-H (2-byte codes, DW 600). Switching
    /// fonts changes the code *width* — the rewrite must decode each
    /// op's string under its own font's code unit.
    fn mixed_fonts(body: &[u8]) -> Vec<u8> {
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R /F2 7 0 R >> >> /Contents 9 0 R >>"
        );
        // /F1 — simple TrueType, 1-byte codes, all widths 600.
        let widths = {
            let w = [600u32; 224];
            format!(
                "[{}]",
                w.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes()
        };
        let mut fd = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        fd.extend_from_slice(&widths);
        fd.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push!(4, &fd);
        push!(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>"
        );
        // /F2 — Type0/Identity-H, 2-byte codes, DW 600.
        push!(
            7,
            b"<< /Type /Font /Subtype /Type0 /BaseFont /KasaneTest \
              /Encoding /Identity-H /DescendantFonts [8 0 R] >>"
        );
        push!(
            8,
            b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /KasaneTest \
              /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
              /FontDescriptor 10 0 R /CIDToGIDMap /Identity /DW 600 >>"
        );
        push!(
            10,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 4 \
              /FontBBox [0 -200 600 800] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>"
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs.extend_from_slice(&enc);
        fs.extend_from_slice(b"\nendstream");
        push!(6, &fs);
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    /// M17.1 — a `Tf` switch from a 1-byte simple font to a 2-byte
    /// Identity-H font: each op's string decodes under its own code
    /// unit. `(AB)` under /F1 and `<0043 0044>` under /F2.
    #[test]
    fn simple_to_identity_h_switch() {
        let d = doc(&mixed_fonts(
            b"BT /F1 24 Tf 20 60 Td (AB) Tj /F2 24 Tf <0043 0044> Tj ET\n",
        ));
        // 'B' under /F1: origin 34.4..40.4. Under /F2 the CID glyphs
        // have overlapping outlines (ink 48.8..65.6 and 63.2..80) so
        // a clean single-code selection is impossible there — remove
        // 'B' plus only the *second* /F2 code (rect 66..82 hits
        // 0x44's 63.2..80 without touching 0x43's 48.8..65.6).
        let r = remove_text(
            &d,
            0,
            &[(34.5, 55.0, 42.0, 80.0), (66.0, 55.0, 82.0, 80.0)],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.codes_removed, 2);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // Both fonts w=600 → kern -600 each, but the /F2 element
        // must keep its *2-byte* kept code (0x43) — octal-escaped
        // bytes `(\000C)`.
        assert!(out.contains("[(A) -600] TJ"), "F1: {out}");
        assert!(out.contains("[(\\000C) -600] TJ"), "F2 keeps 2-byte: {out}");
    }

    /// M17.1 — the rewrite must use the *state at the op*, not the
    /// end-of-stream state. Two `Tj`s with different font sizes: the
    /// second `Tj`'s kern must reflect *its* 10pt size, not the last
    /// `Tf` in the stream.
    ///
    /// Content: `BT /F1 24 Tf 20 60 Td (AB) Tj 10 Tf (C) Tj ET`.
    /// Remove 'B' from the first `Tj` — the kern must be
    /// `-(600·24/24·1000/24)` … more precisely `-(w0 + Tc·1000/Tfs)`
    /// = `-(600 + 0)` for 24pt — but the *bug* is the walker leaves
    /// `font_size = 10` at end-of-stream, so the old code computes
    /// `-(600 + 0·1000/10)` = `-600` — same value here. Use a font
    /// *size* where the kern differs: Tfs scales the kern's *effect*,
    /// not its value, so a size change alone does not expose the bug.
    /// The bug shows when `Tc`/`Tw`/`Tz` or the *font* changes — use
    /// `Tc` here: set `Tc=5` before the first Tj and `Tc=0` after.
    /// The kern for 'B' at `Tc=5` is `-(600 + 5·1000/24)` ≈ `-808.33`;
    /// with the end-of-stream `Tc=0` the buggy code emits `-600`.
    #[test]
    fn state_change_uses_op_state_not_end_of_stream() {
        let d = doc(&one_glyph(
            b"BT /F1 24 Tf 5 Tc 20 60 Td (AB) Tj 0 Tc (C) Tj ET\n",
        ));
        // 'A' advance at Tc=5 is (600/1000·24 + 5)·1 = 19.4 — 'B'
        // origin is x=39.4, ink ~39.4..45.4. Rect (36,55,46,80) hits
        // 'B' only ('A' ink ends ~34.4).
        let r = remove_text(&d, 0, &[(36.0, 55.0, 46.0, 80.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // Kern must carry the op-time `Tc=5`: -(600 + 5·1000/24)
        // = -808.3333. The buggy end-of-stream Tc=0 would give -600.
        assert!(
            out.contains("[(A) -808.3333] TJ"),
            "kern must reflect the op's Tc=5, not the stream-end Tc=0: {out}"
        );
    }

    /// M17.1 — same problem for a `Tf` *font* switch: a second font
    /// with different widths must not change the first `Tj`'s rewrite.
    /// Here the second font is the same file (different name only) so
    /// widths match and the kern is the same either way — instead make
    /// the second `Tf` a *size* change that alters `font_size`, which
    /// the buggy code reads for the *first* Tj's kern. A kern at
    /// `Tfs=10` for 'B' is `-(600 + 0·1000/10)` = `-600` — identical.
    /// So this variant exercises the font *name*/code-width path: use
    /// `Tc` again but verify the *first* op's kern only, proving the
    /// second `Tc` did not bleed backwards.
    #[test]
    fn later_state_does_not_change_earlier_rewrite() {
        // `Tc=9` before (AB), `Tc=1` before (CD) — remove 'B' AND 'C'.
        let d = doc(&one_glyph(
            b"BT /F1 24 Tf 9 Tc 20 60 Td (AB) Tj 1 Tc 20 100 Td (CD) Tj ET\n",
        ));
        // 'A' advance at Tc=9 is 14.4+9 = 23.4 — 'B' origin ≈43.4,
        // ink ~43.4..49.4 (y 60..76.8). 'C' on line 2 sits at
        // origin (40,160), ink ~48.4..54.4 (y 160..176.8) — `Td`
        // moves from the line-start, not the page origin.
        let r = remove_text(
            &d,
            0,
            &[(40.0, 55.0, 50.0, 80.0), (47.0, 155.0, 55.0, 180.0)],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.codes_removed, 2);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // 'B' kern: -(600 + 9·1000/24) = -975. 'C' kern:
        // -(600 + 1·1000/24) = -641.6667. If the walker left Tc=1 the
        // first kern would wrongly be -641.6667 too.
        assert!(
            out.contains("[(A) -975] TJ"),
            "first Tj kern must use its own Tc=9: {out}"
        );
        assert!(
            out.contains("[-641.6667 (D)] TJ"),
            "second Tj kern must use its own Tc=1: {out}"
        );
    }

    /// M17.1 — a `LimitExceeded` from the walk must stay
    /// `LimitExceeded`, not be flattened into `Unsupported` — the
    /// caller distinguishes "out of scope" from "resource bound".
    #[test]
    fn walk_limit_stays_limit_exceeded() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 20 60 Td (ABC) Tj ET\n"));
        // 'A','B' fit; 'C' overflows the code-count limit.
        let l = Limits {
            max_text_codes: 2,
            ..Limits::default()
        };
        let err = remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &l).unwrap_err();
        assert!(
            matches!(err, Error::LimitExceeded { .. }),
            "limit must not be flattened to Unsupported: {err:?}"
        );
    }

    /// Same for `max_diag_items` — a walk that reports more items than
    /// the limit is a resource bound, not an unsupported construct.
    #[test]
    fn diag_items_limit_stays_limit_exceeded() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 20 60 Td (ABC) Tj ET\n"));
        // The first reported glyph already overflows the item limit.
        let l = Limits {
            max_diag_items: 1,
            ..Limits::default()
        };
        let err = remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &l).unwrap_err();
        assert!(
            matches!(err, Error::LimitExceeded { .. }),
            "diag-item limit must not be flattened: {err:?}"
        );
    }

    /// And a `Structure` error — e.g. `Tj` outside `BT`/`ET` — keeps
    /// its own kind rather than being renamed `Unsupported`.
    #[test]
    fn walk_structure_error_keeps_its_kind() {
        let d = doc(&one_glyph(b"/F1 24 Tf 20 60 Td (ABC) Tj\n")); // no BT
        let err = remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default()).unwrap_err();
        assert!(
            matches!(err, Error::Structure(_)),
            "structural error must keep its kind: {err:?}"
        );
    }

    /// Two pages share one `/Contents` — removal on page 1 writes a
    /// *new* stream for that page and leaves page 2 pointing at the
    /// original bytes.
    #[test]
    fn shared_contents_only_edits_the_target_page() {
        // Build a 2-page doc where both /Contents point at stream 9.
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R 7 0 R] /Count 2 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> >> /Contents 9 0 R >>"
        );
        let widths = {
            let mut w = [600u32; 224];
            w[0] = 250;
            format!(
                "[{}]",
                w.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes()
        };
        let mut font_dict = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        font_dict.extend_from_slice(&widths);
        font_dict.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push!(4, &font_dict);
        push!(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>"
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs.extend_from_slice(&enc);
        fs.extend_from_slice(b"\nendstream");
        push!(6, &fs);
        // Page 2 — same Resources, same /Contents stream 9.
        push!(
            7,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> >> /Contents 9 0 R >>"
        );
        let body = b"BT /F1 24 Tf 20 60 Td (ABC) Tj ET\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 10\n0000000000 65535 f \n".to_vec());
        for n in 1..10u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 10 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // Remove 'B' on page 1 — page 2 keeps the original stream.
        let r = remove_text(&d, 0, &[(35.0, 60.0, 38.0, 80.0)], &Limits::default()).unwrap();
        let d2 = doc(&r.output);
        // Page 1 extracts "AC"; page 2 still extracts "ABC".
        let p1 = crate::extract_text(&d2, &d2.pages()[0], &Limits::default()).unwrap();
        let s1: String = p1
            .chars
            .iter()
            .filter_map(|c| c.unicode.as_deref())
            .collect();
        assert_eq!(s1, "AC", "page 1: {s1:?}");
        let p2 = crate::extract_text(&d2, &d2.pages()[1], &Limits::default()).unwrap();
        let s2: String = p2
            .chars
            .iter()
            .filter_map(|c| c.unicode.as_deref())
            .collect();
        assert_eq!(s2, "ABC", "page 2 must keep the shared stream: {s2:?}");
    }

    /// A `gs` anywhere is refused — ExtGState is not interpreted.
    #[test]
    fn extgstate_is_refused() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /ExtGState << /GS1 << /LW 5 >> >> >> /Contents 4 0 R >>"
        );
        let body = b"/GS1 gs\n10 10 m 20 10 l S\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(4, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 5\n0000000000 65535 f \n".to_vec());
        for n in 1..5u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            }
        }
        parts.push(b"trailer\n<< /Size 5 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let err = remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default()).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
    }

    // ---- M18.1 — cumulative budgets ---------------------------------

    /// A single form invocation fits the per-stream `max_operations`
    /// parse cap, but *repeated* invocations accumulate op executions
    /// the parse cap never sees. `max_diag_exec_ops` bounds the total
    /// across page + all form recursion — a form with no text (so
    /// `max_text_codes` can't catch it) must still stop.
    #[test]
    fn cumulative_exec_ops_stops_repeated_invocations() {
        // One form of 5 ops (BT Tf Td Tj ET — the text is inside
        // bounds so it's eligible) invoked 4 times: 4×(5 inner) + 4
        // `Do` = 24 executions. A cap of 20 must fail *cumulative*,
        // even though each stream parses under max_operations easily.
        let d = doc(&form_n_invocations_pdf(
            b"BT /F1 24 Tf 0 0 Td (A) Tj ET\n",
            4,
        ));
        let l = Limits {
            max_diag_exec_ops: 20,
            ..Limits::default()
        };
        let err = remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &l).unwrap_err();
        assert!(
            matches!(
                err,
                Error::LimitExceeded {
                    limit: "max_diag_exec_ops",
                    ..
                }
            ),
            "repeated invocations must hit the cumulative op cap: {err:?}"
        );
    }

    /// An empty form (zero inner ops) still burns one `Do` execution
    /// per invocation — the cumulative cap catches it where
    /// `max_text_codes` sees nothing.
    #[test]
    fn cumulative_exec_ops_counts_empty_invocations() {
        // 10 `/Fm1 Do` of an empty form — 10 executions at page level
        // plus 0 inside. A cap of 5 must fail even though no text
        // and no inner ops ever run.
        let d = doc(&form_n_invocations_pdf(b"", 10));
        let l = Limits {
            max_diag_exec_ops: 5,
            ..Limits::default()
        };
        let err = remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &l).unwrap_err();
        assert!(
            matches!(
                err,
                Error::LimitExceeded {
                    limit: "max_diag_exec_ops",
                    ..
                }
            ),
            "empty-form invocations still cost an op each: {err:?}"
        );
    }

    /// Boundary check: N executions exactly at the cap succeed,
    /// N+1 fails.
    #[test]
    fn cumulative_exec_ops_boundary() {
        // Page: `/Fm1 Do` once (1 op) + form: 5 inner ops = 6 total.
        let d = doc(&form_n_invocations_pdf(
            b"BT /F1 24 Tf 0 0 Td (A) Tj ET\n",
            1,
        ));
        let exact = Limits {
            max_diag_exec_ops: 6,
            ..Limits::default()
        };
        remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &exact).expect("6 ops at cap must pass");
        let over = Limits {
            max_diag_exec_ops: 5,
            ..Limits::default()
        };
        assert!(
            matches!(
                remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &over),
                Err(Error::LimitExceeded {
                    limit: "max_diag_exec_ops",
                    ..
                })
            ),
            "5 < 6 must fail"
        );
    }

    /// Snapshot records are bounded too — many show ops with few
    /// codes must not grow `show_states` without bound. A cap
    /// smaller than the op count (each op still executes fine)
    /// fails as `LimitExceeded` on the *snapshot* budget.
    #[test]
    fn show_state_snapshots_are_bounded() {
        // A form with 5 `Tj`s → 5 snapshots per invocation;
        // 2 invocations = 10 entries (distinct form_paths). A cap of
        // 8 must fail before the second invocation finishes — and a
        // diagnose-only caller recording no snapshots at all is
        // unaffected.
        let d = doc(&form_n_invocations_pdf(
            b"BT /F1 24 Tf 0 0 Td (A) Tj (B) Tj (C) Tj (D) Tj (E) Tj ET\n",
            2,
        ));
        let l = Limits {
            max_show_states: 8,
            ..Limits::default()
        };
        let err = remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &l).unwrap_err();
        assert!(
            matches!(
                err,
                Error::LimitExceeded {
                    limit: "max_show_states",
                    ..
                }
            ),
            "snapshot map must be bounded: {err:?}"
        );
    }

    /// Diagnose-only callers don't need `show_states` at all — the
    /// map records *only* when the walk was requested with snapshot
    /// collection on. `diagnose_redaction` never fills it.
    #[test]
    fn diagnose_does_not_record_snapshots() {
        let d = doc(&form_n_invocations_pdf(
            b"BT /F1 24 Tf 0 0 Td (A) Tj ET\n",
            3,
        ));
        let page = &d.pages()[0];
        let ops = d.page_operations(page).unwrap();
        let rects = [crate::page::Rect {
            llx: 0.0,
            lly: 0.0,
            urx: 200.0,
            ury: 200.0,
        }];
        let limits = Limits::default();
        let mut w = DiagWalker::new_diag_only(&d, page, &limits, &rects);
        w.run(&ops).unwrap();
        assert!(
            w.show_states.is_empty(),
            "diagnose-only walk must not record show_states"
        );
    }

    /// Each generated form clone counts its bytes against
    /// `max_edit_content_bytes` — a small shared form cloned many
    /// times must stop on the *cumulative* budget even though each
    /// stream alone is tiny and the final output is far from
    /// `max_output_bytes`.
    #[test]
    fn generated_content_bytes_are_cumulative() {
        // Form body ~32 bytes → clone body ~38 bytes; 3 invocations
        // = ~114 generated bytes + page stream. A cap below the
        // total but above one stream must fail partway.
        let d = doc(&form_n_invocations_pdf(
            b"BT /F1 24 Tf 0 0 Td (ABC) Tj ET\n",
            3,
        ));
        // A rect that hits 'B' under all 3 identical invocations.
        let l = Limits {
            max_edit_content_bytes: 40, // ~1 clone + some page stream
            ..Limits::default()
        };
        let err = remove_text(&d, 0, &[(14.5, 0.0, 21.0, 20.0)], &l).unwrap_err();
        assert!(
            matches!(
                err,
                Error::LimitExceeded {
                    limit: "max_edit_content_bytes",
                    ..
                }
            ),
            "cumulative generated bytes must refuse: {err:?}"
        );
    }

    // ---- M18.2 — BoundedWriter unit tests ----------------------------

    /// A `BoundedWriter` stops *before* the byte that would exceed
    /// the budget — the failed `push` leaves the buffer unchanged,
    /// so a caller that ignores the error still holds only the
    /// allowed prefix.
    #[test]
    fn bounded_writer_refuses_before_exceeding() {
        let limits = Limits {
            max_edit_content_bytes: 10,
            ..Limits::default()
        };
        let mut w = BoundedWriter::new(&limits, 0);
        // 10 bytes at the cap succeed.
        for _ in 0..10 {
            w.push(b'x').unwrap();
        }
        assert_eq!(w.buf.len(), 10);
        // The 11th fails *before* landing — buf stays 10.
        assert!(matches!(
            w.push(b'x'),
            Err(Error::LimitExceeded {
                limit: "max_edit_content_bytes",
                ..
            })
        ));
        assert_eq!(w.buf.len(), 10);
        assert_eq!(&w.buf, &[b'x'; 10]);
    }

    /// A slice add checks the *whole* length up front — a 5-byte
    /// slice into a 6-byte buffer must fail atomically, leaving the
    /// original 6 bytes untouched.
    #[test]
    fn bounded_writer_slice_add_is_atomic() {
        let limits = Limits {
            max_edit_content_bytes: 10,
            ..Limits::default()
        };
        let mut w = BoundedWriter::new(&limits, 0);
        w.extend_from_slice(b"123456").unwrap(); // 6 used
        assert!(matches!(
            w.extend_from_slice(b"78901"), // +5 → 11 > 10
            Err(Error::LimitExceeded {
                limit: "max_edit_content_bytes",
                ..
            })
        ));
        assert_eq!(w.buf.len(), 6);
        assert_eq!(&w.buf, b"123456");
        // A 4-byte slice fits exactly.
        w.extend_from_slice(b"7890").unwrap();
        assert_eq!(&w.buf, b"1234567890");
    }

    /// `generated_so_far` shifts the cumulative bound — a second
    /// stream sees only the bytes still left in
    /// `max_edit_content_bytes`.
    #[test]
    fn bounded_writer_respects_cumulative_budget() {
        let limits = Limits {
            max_edit_content_bytes: 20,
            ..Limits::default()
        };
        // First stream used 15; this one gets 5 before hitting the cap.
        let mut w = BoundedWriter::new(&limits, 15);
        w.extend_from_slice(b"12345").unwrap(); // 5 used, 15+5=20 total
        assert!(matches!(
            w.push(b'x'),
            Err(Error::LimitExceeded {
                limit: "max_edit_content_bytes",
                ..
            })
        ));
        assert_eq!(w.buf.len(), 5);
    }

    /// `max_output_bytes` (single-stream) is checked alongside the
    /// cumulative bound — a tight per-stream cap fires even when
    /// `max_edit_content_bytes` is generous.
    #[test]
    fn bounded_writer_single_stream_cap() {
        let limits = Limits {
            max_edit_content_bytes: 1000,
            max_output_bytes: 5,
            ..Limits::default()
        };
        let mut w = BoundedWriter::new(&limits, 0);
        w.extend_from_slice(b"12345").unwrap(); // 5 used, under both caps
        assert!(matches!(
            w.push(b'x'),
            Err(Error::LimitExceeded {
                limit: "max_output_bytes",
                ..
            })
        ));
        assert_eq!(w.buf.len(), 5);
    }

    /// A `Tj` with a huge literal string stops mid-write — the
    /// `BoundedWriter` refuses the *next* byte, not after the whole
    /// op is rendered. A 100-byte string into a 10-byte budget must
    /// fail on the 11th byte, leaving 10 bytes in the buffer.
    #[test]
    fn tj_with_long_string_stops_mid_write() {
        let limits = Limits {
            max_edit_content_bytes: 10,
            ..Limits::default()
        };
        let mut w = BoundedWriter::new(&limits, 0);
        let op = Operation::ShowText(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_vec()); // 40 a's
                                                                                            // The `(` + 10 a's = 11 bytes — the 11th `a` refuses at
                                                                                            // budget 10. The `)` and ` Tj` never land.
        assert!(matches!(
            write_op(&op, &mut w),
            Err(Error::LimitExceeded {
                limit: "max_edit_content_bytes",
                ..
            })
        ));
        assert_eq!(w.buf.len(), 10);
        assert_eq!(&w.buf, b"(aaaaaaaaa"); // `(` + 9 a's = 10
    }

    /// A `TJ` array with many numeric elements stops mid-write —
    /// each ` ` separator and each `Kern` number is checked before
    /// landing.
    #[test]
    fn tj_many_kerns_stops_mid_write() {
        let limits = Limits {
            max_edit_content_bytes: 15,
            ..Limits::default()
        };
        let mut w = BoundedWriter::new(&limits, 0);
        let op = Operation::ShowTextAdjusted(vec![
            TJItem::Kern(100.5), // "100.5" = 5 bytes
            TJItem::Kern(200.5), // "200.5" = 5 bytes
            TJItem::Kern(300.5), // "300.5" = 5 bytes
        ]);
        let r = write_op(&op, &mut w);
        assert!(matches!(
            r,
            Err(Error::LimitExceeded {
                limit: "max_edit_content_bytes",
                ..
            })
        ));
        // `[` + `100.5` + ` ` + `200.5` + ` ` = 13 bytes; the next
        // `300.5` would push to 19 > 15, so it refuses *before*
        // landing. The buffer holds the allowed prefix.
        assert_eq!(w.buf.len(), 13);
        assert_eq!(&w.buf, b"[100.5 200.5 ");
    }

    /// Escaped characters in a literal string expand to 4 bytes
    /// (`\xNN`) — the check sees the *expanded* length, so a string
    /// of 3 control bytes needs 12 budget bytes + 2 for `()`.
    #[test]
    fn escaped_chars_count_expanded_length() {
        let limits = Limits {
            max_edit_content_bytes: 13, // `(` + 3×`\xNN` = 13; `)` refuses
            ..Limits::default()
        };
        let mut w = BoundedWriter::new(&limits, 0);
        let op = Operation::ShowText(vec![0x01, 0x02, 0x03]);
        let r = write_op(&op, &mut w);
        assert!(matches!(
            r,
            Err(Error::LimitExceeded {
                limit: "max_edit_content_bytes",
                ..
            })
        ));
        // `(` + `\001` + `\002` + `\003` = 13 bytes; `)` would be 14 —
        // the `)` push refuses at 13, leaving 13 in the buffer.
        assert_eq!(w.buf.len(), 13);
        assert_eq!(&w.buf, b"(\\001\\002\\003");
    }
}
