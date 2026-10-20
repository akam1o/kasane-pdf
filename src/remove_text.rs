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
//! `docs/coverage.md` for the full scope list.
//!
//! Scope — a page is *refused* (typed error) rather than half-edited
//! when any of these appear in its content or page dict:
//!
//! - `/Annots` — annotations are out of scope (the diagnosis sees
//!   them; removal does not reason about their content).
//! - Any `Do`, `sh`, `gs`, Pattern colour space, `BI`/`ID`/`EI`
//!   inline image, or `/ActualText`/`OC` marked content — the
//!   diagnosis already marks these; removal refuses on the first one
//!   rather than continue past unknown state.
//! - Any text-rendering mode other than `Tr 0` (fill) — stroke,
//!   clip, and invisible text are all refused; `w = 0` hairlines
//!   never reach here (diagnosis reports them `uncertain`).
//! - A `Form` XObject `Do`, even at page level — form content is out
//!   of scope (the diagnosis walks it; removal refuses).
//! - Unsupported or unresolvable fonts, non-finite transforms, and
//!   any other construct that makes the diagnosis `incomplete`,
//!   `uncertain`, or `blocker`-carrying — those are refused, never
//!   silently treated as "nothing to remove".
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
use crate::object::Object;
use crate::redact_diag::{check_rect, page_dict_entry, DiagWalker, Site, TargetDetail, TargetKind};
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
    if let Err(e) = walker.run(&ops) {
        // A mid-stream failure is not "nothing to remove" — the page
        // could not be fully walked, so removal refuses.
        return Err(Error::Unsupported {
            feature: "text removal on an un-walkable page",
            detail: format!("content walk failed: {e}"),
        });
    }
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
    // A `Do` — even when the diagnosis handles it — is out of scope
    // for M17 (form text is not edited). Refuse on the first one.
    if let Some((i, _)) = ops
        .iter()
        .enumerate()
        .find(|(_, o)| matches!(o, Operation::Do(_) | Operation::Shading(_)))
    {
        return Err(Error::Unsupported {
            feature: "text removal on a page with Do/sh",
            detail: format!("op #{i} — XObject/shading content is out of scope"),
        });
    }

    // ---- collect codes to remove ------------------------------------
    // (op_index → sorted code_index list). A code's `hits` tell us
    // its ink bounds overlapped a user rect — the *filled* bounds of
    // a `Tr 0` glyph, edge-touching counts as a hit.
    let mut to_remove: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for t in &walker.targets {
        if t.kind == TargetKind::Text && !t.hits.is_empty() {
            if let TargetDetail::Text { render_mode, .. } = &t.detail {
                // Defensive: the walker already refuses non-fill
                // modes, but a caller-supplied Targets could pass a
                // stroked glyph — refuse rather than guess.
                if *render_mode != 0u8 {
                    return Err(Error::Unsupported {
                        feature: "text removal of non-fill-mode text",
                        detail: format!("Tr {render_mode} is not plain fill"),
                    });
                }
            }
            let op = t.site.op_index;
            let ci = t
                .site
                .code_index
                .ok_or_else(|| Error::Structure("text target without code_index".into()))?;
            to_remove.entry(op).or_default().push(ci);
        }
    }

    // ---- rewrite the content stream ---------------------------------
    // Build a new ops vec with text ops rewritten; everything else
    // passes through unchanged. `Tj`/`'`/`"` are converted to `TJ`
    // when they lose codes so the survivors' positions are preserved.
    let mut new_ops: Vec<Operation> = Vec::with_capacity(ops.len());
    let mut removed_sites: Vec<Site> = Vec::new();
    let mut codes_removed: u64 = 0;
    for (op_index, op) in ops.iter().enumerate() {
        match op {
            Operation::ShowText(bytes) => {
                let rem = to_remove.get(&op_index);
                match rem {
                    None => new_ops.push(op.clone()),
                    Some(idxs) => {
                        let (new_items, n_removed, sites) =
                            rewrite_show_text(&walker, bytes, idxs, op_index, None)?;
                        codes_removed += n_removed;
                        removed_sites.extend(sites);
                        new_ops.extend(new_items);
                    }
                }
            }
            Operation::ShowTextAdjusted(items) => {
                let rem = to_remove.get(&op_index);
                match rem {
                    None => new_ops.push(op.clone()),
                    Some(idxs) => {
                        let (new_items, n_removed, sites) =
                            rewrite_tj(&walker, items, idxs, op_index)?;
                        codes_removed += n_removed;
                        removed_sites.extend(sites);
                        new_ops.extend(new_items);
                    }
                }
            }
            // `'` and `"` carry side effects beyond Tj — expand them
            // to `T*` + `Tj` (and `Tw`/`Tc` for `"`) so the rewrite
            // never drops the leading/spacing update. (A `'` or `"`
            // whose text is fully removed still needs its `T*`.)
            Operation::NextLineShowText(text) => {
                let rem = to_remove.get(&op_index);
                match rem {
                    None => new_ops.push(op.clone()),
                    Some(idxs) => {
                        // `'` ≡ T* + Tj — emit the T* unconditionally
                        // so the line move survives an empty Tj.
                        new_ops.push(Operation::NextLine);
                        let (new_items, n_removed, sites) =
                            rewrite_show_text(&walker, text, idxs, op_index, None)?;
                        codes_removed += n_removed;
                        removed_sites.extend(sites);
                        new_ops.extend(new_items);
                    }
                }
            }
            Operation::SetSpacingNextLineShowText {
                word_spacing,
                char_spacing,
                text,
            } => {
                let rem = to_remove.get(&op_index);
                match rem {
                    None => new_ops.push(op.clone()),
                    Some(idxs) => {
                        // `"` ≡ Tw + Tc + T* + Tj — emit the spacing
                        // and T* unconditionally.
                        new_ops.push(Operation::SetWordSpacing(*word_spacing));
                        new_ops.push(Operation::SetCharSpacing(*char_spacing));
                        new_ops.push(Operation::NextLine);
                        let (new_items, n_removed, sites) = rewrite_show_text(
                            &walker,
                            text,
                            idxs,
                            op_index,
                            Some((*word_spacing, *char_spacing)),
                        )?;
                        codes_removed += n_removed;
                        removed_sites.extend(sites);
                        new_ops.extend(new_items);
                    }
                }
            }
            _ => new_ops.push(op.clone()),
        }
    }

    // Nothing removed → nothing to write. Report an empty-edit result
    // (still a valid PDF — the caller sees `codes_removed == 0`).
    let codes_removed = codes_removed;

    // ---- serialise the new stream -----------------------------------
    let new_body = serialize_ops(&new_ops, limits)?;
    // The replacement Contents is a synthetic object; the page dict
    // is updated to point at it. Other pages that share the original
    // stream keep it untouched.
    let mut edit = Edit::new(doc, limits.clone())?;
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
            "scope: page-level content only — no Form/Annots/metadata/structure".into(),
        ],
    })
}

/// Rewrite one `Tj`/`'`/`"` string with `remove` codes taken out —
/// emits a `TJ` op (or nothing when every code went) whose numeric
/// elements carry each removed code's advance so following glyphs
/// land where they did. `cs_extra` is `Some((Tw, Tc))` for `"` —
/// its spacing values were already emitted as separate ops.
///
/// Returns `(ops, codes_removed, sites)` — `ops` is empty when the
/// whole string was removed.
fn rewrite_show_text(
    walker: &DiagWalker,
    bytes: &[u8],
    remove: &[usize],
    op_index: usize,
    cs_extra: Option<(f64, f64)>,
) -> Result<(Vec<Operation>, u64, Vec<Site>), Error> {
    let font = walker
        .gs
        .text
        .font
        .as_ref()
        .ok_or_else(|| Error::Structure("Tj before Tf — no font selected".into()))?;
    let t = &walker.gs.text;
    // The advance a removed code contributes is
    //   (w0/1000 × Tfs + Tc [+ Tw if space]) × Th
    // which a TJ numeric element reproduces as `-advance × 1000 /
    // (Tfs × Th)` — i.e. -(w0 + (Tc[+Tw])×1000/Tfs) — so the
    // surviving codes keep their original positions.
    let font_size = t.font_size;
    let h_scale = t.h_scale;
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
    // `cs_extra` is `Some((Tw, Tc))` only for `"` — that op *sets*
    // the spacing values (they replace `t.word_spacing`/
    // `t.char_spacing` for the shown string), so use them verbatim
    // rather than adding to the walker's current state.
    let (word_spacing, char_spacing) = match cs_extra {
        Some((w, c)) => (w, c),
        None => (t.word_spacing, t.char_spacing),
    };

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
    // carrying its summed advance. `pos` inside `bytes` is tracked so
    // each kept string is the original byte slice.
    //
    // A removed code's advance (user space) is
    //   ((w0/1000)·Tfs + Tc [+Tw if space]) · Th
    // and a TJ numeric `n` moves the next glyph by `-n·Tfs·Th/1000`.
    // So `n = -(w0 + (Tc[+Tw])·1000/Tfs)` — `h_scale` cancels and must
    // not appear in the formula (it is *not* part of the TJ-number
    // semantics; §9.4.4 shows the adjustment is `w0 - n/1000` in glyph
    // space before `Tfs·Th` is applied).
    // Split `codes` into kept runs and removed runs — each kept run
    // becomes a string element, each removed run a numeric element
    // carrying its summed advance. The numeric element goes *between*
    // the kept runs so the surviving codes land where they did:
    // `(ABC)` removing 'B' → `[(A) <advB> (C)]`, not `[(A)(C) <advB>]`.
    //
    // A removed code's advance (user space) is
    //   ((w0/1000)·Tfs + Tc [+Tw if space]) · Th
    // and a TJ numeric `n` moves the next glyph by `-n·Tfs·Th/1000`.
    // So `n = -(w0 + (Tc[+Tw])·1000/Tfs)` — `h_scale` cancels and must
    // not appear in the formula (it is *not* part of the TJ-number
    // semantics; §9.4.4 shows the adjustment is `w0 - n/1000` in glyph
    // space before `Tfs·Th` is applied).
    let mut items: Vec<TJItem> = Vec::new();
    let mut sites: Vec<Site> = Vec::new();
    let mut codes_removed: u64 = 0;
    let mut run_start: Option<usize> = None;
    let mut removed_advance = 0.0f64;
    for (ci, (code, byte_pos)) in codes.iter().enumerate() {
        if remove.contains(&ci) {
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
            sites.push(walker.text_site_at(op_index, ci));
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
/// through unchanged.
fn rewrite_tj(
    walker: &DiagWalker,
    items: &[TJItem],
    remove: &[usize],
    op_index: usize,
) -> Result<(Vec<Operation>, u64, Vec<Site>), Error> {
    let font = walker
        .gs
        .text
        .font
        .as_ref()
        .ok_or_else(|| Error::Structure("TJ before Tf — no font selected".into()))?;
    let t = &walker.gs.text;
    let font_size = t.font_size;
    let h_scale = t.h_scale;
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
    let char_spacing = t.char_spacing;
    let word_spacing = t.word_spacing;

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
                    if remove.contains(&global) {
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
                        sites.push(walker.text_site_at(op_index, global));
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

/// Serialise one op to content-stream bytes — a small subset of the
/// full grammar, exactly what `remove_text` emits.
fn serialize_ops(ops: &[Operation], limits: &Limits) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut ops_written: usize = 0;
    for op in ops {
        ops_written += 1;
        if ops_written > limits.max_operations {
            return Err(Error::LimitExceeded {
                limit: "max_operations",
                max: limits.max_operations as u64,
            });
        }
        write_op(op, &mut out, limits)?;
        out.push(b'\n');
        if out.len() as u64 > limits.max_output_bytes {
            return Err(Error::LimitExceeded {
                limit: "max_output_bytes",
                max: limits.max_output_bytes,
            });
        }
    }
    Ok(out)
}

/// Serialise a single `Operation` — supports every variant the
/// removal path can emit. Literal-string bytes are written verbatim
/// with `(`/`)`/`\` escaped (the same encoding the parser accepts).
fn write_op(op: &Operation, out: &mut Vec<u8>, _limits: &Limits) -> Result<(), Error> {
    match op {
        Operation::SaveState => out.extend_from_slice(b"q"),
        Operation::RestoreState => out.extend_from_slice(b"Q"),
        Operation::ConcatMatrix(m) => {
            for v in [m.a, m.b, m.c, m.d, m.e, m.f] {
                out.extend_from_slice(fmt_num(v).as_bytes());
                out.push(b' ');
            }
            out.extend_from_slice(b"cm");
        }
        Operation::SetLineWidth(w) => {
            out.extend_from_slice(fmt_num(*w).as_bytes());
            out.extend_from_slice(b" w");
        }
        Operation::SetLineCap(c) => {
            out.extend_from_slice(c.to_string().as_bytes());
            out.extend_from_slice(b" J");
        }
        Operation::SetLineJoin(j) => {
            out.extend_from_slice(j.to_string().as_bytes());
            out.extend_from_slice(b" j");
        }
        Operation::SetMiterLimit(m) => {
            out.extend_from_slice(fmt_num(*m).as_bytes());
            out.extend_from_slice(b" M");
        }
        Operation::SetDash { dashes, phase } => {
            out.push(b'[');
            for (i, v) in dashes.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                out.extend_from_slice(fmt_num(*v).as_bytes());
            }
            out.extend_from_slice(b"] ");
            out.extend_from_slice(fmt_num(*phase).as_bytes());
            out.extend_from_slice(b" d");
        }
        Operation::SetRenderingIntent(n) => {
            out.push(b'/');
            out.extend_from_slice(n);
            out.extend_from_slice(b" ri");
        }
        Operation::SetFlatness(i) => {
            out.extend_from_slice(fmt_num(*i).as_bytes());
            out.extend_from_slice(b" i");
        }
        Operation::SetExtGState(n) => {
            out.push(b'/');
            out.extend_from_slice(n);
            out.extend_from_slice(b" gs");
        }
        Operation::MoveTo(x, y) => {
            out.extend_from_slice(fmt_num(*x).as_bytes());
            out.push(b' ');
            out.extend_from_slice(fmt_num(*y).as_bytes());
            out.extend_from_slice(b" m");
        }
        Operation::LineTo(x, y) => {
            out.extend_from_slice(fmt_num(*x).as_bytes());
            out.push(b' ');
            out.extend_from_slice(fmt_num(*y).as_bytes());
            out.extend_from_slice(b" l");
        }
        Operation::CurveTo(x1, y1, x2, y2, x3, y3) => {
            for (i, v) in [x1, y1, x2, y2, x3, y3].iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                out.extend_from_slice(fmt_num(**v).as_bytes());
            }
            out.extend_from_slice(b" c");
        }
        Operation::CurveToV(x2, y2, x3, y3) => {
            for (i, v) in [x2, y2, x3, y3].iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                out.extend_from_slice(fmt_num(**v).as_bytes());
            }
            out.extend_from_slice(b" v");
        }
        Operation::CurveToY(x1, y1, x3, y3) => {
            for (i, v) in [x1, y1, x3, y3].iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                out.extend_from_slice(fmt_num(**v).as_bytes());
            }
            out.extend_from_slice(b" y");
        }
        Operation::ClosePath => out.extend_from_slice(b"h"),
        Operation::Rectangle(x, y, w, h) => {
            for (i, v) in [x, y, w, h].iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                out.extend_from_slice(fmt_num(**v).as_bytes());
            }
            out.extend_from_slice(b" re");
        }
        Operation::Stroke => out.extend_from_slice(b"S"),
        Operation::CloseAndStroke => out.extend_from_slice(b"s"),
        Operation::Fill => out.extend_from_slice(b"f"),
        Operation::FillEvenOdd => out.extend_from_slice(b"f*"),
        Operation::FillAndStroke => out.extend_from_slice(b"B"),
        Operation::FillAndStrokeEvenOdd => out.extend_from_slice(b"B*"),
        Operation::CloseFillAndStroke => out.extend_from_slice(b"b"),
        Operation::CloseFillAndStrokeEvenOdd => out.extend_from_slice(b"b*"),
        Operation::EndPath => out.extend_from_slice(b"n"),
        Operation::Clip => out.extend_from_slice(b"W"),
        Operation::ClipEvenOdd => out.extend_from_slice(b"W*"),
        Operation::BeginText => out.extend_from_slice(b"BT"),
        Operation::EndText => out.extend_from_slice(b"ET"),
        Operation::SetTextFont { font, size } => {
            out.push(b'/');
            out.extend_from_slice(font);
            out.push(b' ');
            out.extend_from_slice(fmt_num(*size).as_bytes());
            out.extend_from_slice(b" Tf");
        }
        Operation::SetCharSpacing(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes());
            out.extend_from_slice(b" Tc");
        }
        Operation::SetWordSpacing(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes());
            out.extend_from_slice(b" Tw");
        }
        Operation::SetHorizontalScaling(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes());
            out.extend_from_slice(b" Tz");
        }
        Operation::SetTextLeading(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes());
            out.extend_from_slice(b" TL");
        }
        Operation::SetTextRenderingMode(m) => {
            out.extend_from_slice(m.to_string().as_bytes());
            out.extend_from_slice(b" Tr");
        }
        Operation::SetTextRise(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes());
            out.extend_from_slice(b" Ts");
        }
        Operation::MoveTextPosition(x, y) => {
            out.extend_from_slice(fmt_num(*x).as_bytes());
            out.push(b' ');
            out.extend_from_slice(fmt_num(*y).as_bytes());
            out.extend_from_slice(b" Td");
        }
        Operation::MoveTextPositionAndSetLeading(x, y) => {
            out.extend_from_slice(fmt_num(*x).as_bytes());
            out.push(b' ');
            out.extend_from_slice(fmt_num(*y).as_bytes());
            out.extend_from_slice(b" TD");
        }
        Operation::SetTextMatrix(m) => {
            for v in [m.a, m.b, m.c, m.d, m.e, m.f] {
                out.extend_from_slice(fmt_num(v).as_bytes());
                out.push(b' ');
            }
            out.extend_from_slice(b"Tm");
        }
        Operation::NextLine => out.extend_from_slice(b"T*"),
        Operation::ShowText(bytes) => {
            write_literal_string(bytes, out);
            out.extend_from_slice(b" Tj");
        }
        Operation::ShowTextAdjusted(items) => {
            out.push(b'[');
            let mut first = true;
            for it in items {
                if !first {
                    out.push(b' ');
                }
                first = false;
                match it {
                    TJItem::Text(b) => write_literal_string(b, out),
                    TJItem::Kern(n) => out.extend_from_slice(fmt_num(*n).as_bytes()),
                }
            }
            out.extend_from_slice(b"] TJ");
        }
        Operation::NextLineShowText(text) => {
            write_literal_string(text, out);
            out.extend_from_slice(b" '");
        }
        Operation::SetSpacingNextLineShowText {
            word_spacing,
            char_spacing,
            text,
        } => {
            out.extend_from_slice(fmt_num(*word_spacing).as_bytes());
            out.push(b' ');
            out.extend_from_slice(fmt_num(*char_spacing).as_bytes());
            out.push(b' ');
            write_literal_string(text, out);
            out.extend_from_slice(b" \"");
        }
        Operation::Do(n) => {
            out.push(b'/');
            out.extend_from_slice(n);
            out.extend_from_slice(b" Do");
        }
        Operation::Shading(n) => {
            out.push(b'/');
            out.extend_from_slice(n);
            out.extend_from_slice(b" sh");
        }
        Operation::MarkedContentPoint(n) => {
            out.push(b'/');
            out.extend_from_slice(n);
            out.extend_from_slice(b" MP");
        }
        Operation::DesignatedMarkedContentPoint { tag, properties } => {
            out.push(b'/');
            out.extend_from_slice(tag);
            out.push(b' ');
            write_object_shallow(properties, out);
            out.extend_from_slice(b" DP");
        }
        Operation::BeginMarkedContent(n) => {
            out.push(b'/');
            out.extend_from_slice(n);
            out.extend_from_slice(b" BMC");
        }
        Operation::BeginMarkedContentWithProps { tag, properties } => {
            out.push(b'/');
            out.extend_from_slice(tag);
            out.push(b' ');
            write_object_shallow(properties, out);
            out.extend_from_slice(b" BDC");
        }
        Operation::EndMarkedContent => out.extend_from_slice(b"EMC"),
        Operation::BeginCompatibility => out.extend_from_slice(b"BX"),
        Operation::EndCompatibility => out.extend_from_slice(b"EX"),
        // Unknown ops inside BX…EX round-trip verbatim — the operands
        // were parsed objects, re-emitted with the same serialiser.
        Operation::Unknown { operator, operands } => {
            for o in operands {
                write_object_shallow(o, out);
                out.push(b' ');
            }
            out.extend_from_slice(operator);
        }
        // Non-text-state ops the walker refuses on are unreachable —
        // `remove_text` bails before this point. Anything not listed
        // above falls through to a typed error rather than a guess.
        _ => {
            return Err(Error::Unsupported {
                feature: "content rewrite",
                detail: format!("op '{}' is outside the M17 rewrite set", op.name()),
            })
        }
    }
    Ok(())
}

/// Literal PDF string: `(…)` with `(`/`)`/`\` and control bytes
/// escaped. Non-ASCII bytes are written as `\xNN` octal escapes so
/// the output stays ASCII-clean and re-parses exactly.
fn write_literal_string(bytes: &[u8], out: &mut Vec<u8>) {
    out.push(b'(');
    for &b in bytes {
        match b {
            b'(' | b')' | b'\\' => {
                out.push(b'\\');
                out.push(b);
            }
            0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f | 0x7f..=0xff => {
                out.extend_from_slice(format!("\\{:03o}", b).as_bytes());
            }
            _ => out.push(b),
        }
    }
    out.push(b')');
}

/// Serialise a scalar/array/dict object shallowly — used for
/// marked-content properties and `Unknown` operands. Streams are not
/// representable here (none appear as operands).
fn write_object_shallow(o: &Object, out: &mut Vec<u8>) {
    match o {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Object::Integer(n) => out.extend_from_slice(n.to_string().as_bytes()),
        Object::Real(n) => out.extend_from_slice(fmt_num(*n).as_bytes()),
        Object::Name(n) => {
            out.push(b'/');
            out.extend_from_slice(n);
        }
        Object::String(s) => write_literal_string(s, out),
        Object::Array(a) => {
            out.push(b'[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                write_object_shallow(x, out);
            }
            out.push(b']');
        }
        Object::Dict(d) => {
            out.extend_from_slice(b"<<");
            for (k, v) in d {
                out.push(b' ');
                out.push(b'/');
                out.extend_from_slice(k);
                out.push(b' ');
                write_object_shallow(v, out);
            }
            out.extend_from_slice(b" >>");
        }
        Object::Ref(r) => {
            out.extend_from_slice(format!("{} {} R", r.number, r.generation).as_bytes());
        }
        Object::Stream { .. } => {
            // Unreachable — an operand is never a stream.
            out.extend_from_slice(b"null");
        }
    }
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
        let err = remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default()).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "Do must refuse, got {err:?}"
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

    /// An invisible (Tr 3) text page is refused — invisible text is
    /// not evaluated.
    #[test]
    fn invisible_text_is_refused() {
        let d = doc(&one_glyph(b"BT /F1 24 Tf 20 60 Td 3 Tr (ABC) Tj ET\n"));
        let err = remove_text(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default()).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
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
}
