//! `redact`: the positions of removed text.
//!
//! Removing codes keeps the line's layout: what follows lands where it
//! did. Done naively that keeps the removed text's positioning — each
//! removed show's `Td`/`Tm`, its advance as a `TJ` number, the kerns
//! between removed glyphs — and with them the removed glyphs' widths
//! and positions, from which text can be guessed. This pass rewrites a
//! redacted content stream so that between two shows that still draw
//! something, the text positioning is one translation: the position of
//! the next kept glyph and nothing before it.
//!
//! Within a text object, a run of operators between kept shows is
//! folded: its positioning (`Td`, `TD`, `T*`, `Tm`) and the shows that
//! draw nothing (only `TJ` numbers, empty strings) compose into one
//! translation of the line matrix and one of the text matrix, and are
//! written as one `[K] TJ` (no line move), one `Td` or one `Tm`. That
//! leaves the line matrix offset by the run's trailing advance, so the
//! next relative move (`Td`, `TD`, `T*`, `'`, `"`) is preceded by the
//! inverse `Td`. Everything else in the run (colour, marked content,
//! text state, `q`/`Q`) is kept in order: none of it moves text. A
//! kept show's leading and trailing `TJ` numbers belong to the runs
//! around it; adjacent numbers inside it are summed.
//!
//! All of this is exact composition of translations. What the output
//! still records is what the kept text needs: where each kept show
//! starts (also visible in any rendering) and, when the leading is
//! inherited and unknown here, the number of `T*` line moves.

use crate::content::{Operation, TJItem};
use crate::error::Error;
use crate::matrix::Matrix;

/// Text state this stream starts with, `None` where inherited and
/// unknown (a form's).
#[derive(Clone, Copy)]
pub(crate) struct TextStart {
    pub(crate) font_size: Option<f64>,
    pub(crate) h_scale: Option<f64>,
    pub(crate) leading: Option<f64>,
}

impl TextStart {
    /// A page's content stream (§9.3.1: Tz 100 %, TL 0; no font yet).
    pub(crate) const PAGE: TextStart = TextStart {
        font_size: None,
        h_scale: Some(1.0),
        leading: Some(0.0),
    };
    /// A form's: everything inherited.
    pub(crate) const FORM: TextStart = TextStart {
        font_size: None,
        h_scale: None,
        leading: None,
    };
}

fn unsupported(detail: &str) -> Error {
    Error::Unsupported {
        feature: "positions of removed text",
        detail: detail.into(),
    }
}

#[derive(Clone, Copy)]
struct Scale {
    font_size: Option<f64>,
    h_scale: Option<f64>,
    leading: Option<f64>,
}

impl Scale {
    /// Tfs × Th: a `TJ` number `n` moves the text matrix by
    /// `−n / 1000 × Tfs × Th` (§9.4.4).
    fn factor(&self) -> Option<f64> {
        Some(self.font_size? * self.h_scale?)
    }
}

/// A run being folded.
#[derive(Default)]
struct Run {
    /// Operators that do not move text, in order.
    kept: Vec<Operation>,
    /// The run set the text matrix absolutely (`Tm`).
    absolute: Option<Matrix>,
    /// Translation of the line matrix since the run start (or since
    /// `absolute`), excluding `unknown_tstar` moves.
    line: (f64, f64),
    /// `T*` moves whose leading is unknown here (kept as `T*`).
    unknown_tstar: usize,
    /// Any line move (`Td`, `TD`, `T*`, `Tm`).
    moved: bool,
    /// `TJ` numbers since the last line move (or the run start), in
    /// units of the current Tfs × Th.
    kern: f64,
}

struct Folder {
    out: Vec<Operation>,
    scale: Scale,
    saved: Vec<Scale>,
    in_text: bool,
    run: Run,
    /// The line matrix is ahead of the original by this text-space x
    /// translation (the last folded run's trailing advance): undone
    /// before the next relative move.
    ahead: f64,
}

impl Folder {
    fn rescale(&mut self, new: Scale) -> Result<(), Error> {
        if self.run.kern != 0.0 {
            match (self.scale.factor(), new.factor()) {
                (Some(a), Some(b)) if b != 0.0 && b.is_finite() => self.run.kern *= a / b,
                _ => {
                    return Err(unsupported(
                        "a TJ advance under a text scale not known here",
                    ))
                }
            }
        }
        self.scale = new;
        Ok(())
    }

    /// A relative line move by `t` (text space, before the line matrix).
    fn line_move(&mut self, t: (f64, f64)) {
        if self.ahead != 0.0 {
            self.run.line.0 -= self.ahead;
            self.ahead = 0.0;
        }
        self.run.line.0 += t.0;
        self.run.line.1 += t.1;
        self.run.moved = true;
        self.run.kern = 0.0;
    }

    fn tstar(&mut self) {
        match self.scale.leading {
            Some(l) => self.line_move((0.0, -l)),
            None => {
                // Kept as a `T*`; still undo the offset first.
                self.line_move((0.0, 0.0));
                self.run.unknown_tstar += 1;
            }
        }
    }

    fn kerns(&mut self, items: &[TJItem]) {
        for i in items {
            if let TJItem::Kern(n) = i {
                self.run.kern += n;
            }
        }
    }

    /// Write the folded run. `before_show`: a kept show follows (else
    /// the text object ends and positions no longer matter).
    fn flush(&mut self, before_show: bool) -> Result<(), Error> {
        let run = std::mem::take(&mut self.run);
        self.out.extend(run.kept);
        if !before_show {
            return Ok(());
        }
        for _ in 0..run.unknown_tstar {
            self.out.push(Operation::NextLine);
        }
        if !run.moved {
            if run.kern != 0.0 {
                self.out
                    .push(Operation::ShowTextAdjusted(vec![TJItem::Kern(run.kern)]));
            }
            return Ok(());
        }
        let dx = if run.kern == 0.0 {
            0.0
        } else {
            let f = self
                .scale
                .factor()
                .ok_or_else(|| unsupported("a TJ advance under a text scale not known here"))?;
            -run.kern / 1000.0 * f
        };
        if !dx.is_finite() {
            return Err(unsupported("a non-finite advance"));
        }
        let v = (run.line.0 + dx, run.line.1);
        match run.absolute {
            Some(m) => self.out.push(Operation::SetTextMatrix(
                Matrix::new(1.0, 0.0, 0.0, 1.0, v.0, v.1).concat(m),
            )),
            None => self.out.push(Operation::MoveTextPosition(v.0, v.1)),
        }
        self.ahead = dx;
        Ok(())
    }

    fn op(&mut self, op: &Operation) -> Result<(), Error> {
        use Operation as O;
        // Text state, saved and restored with the graphics state.
        match op {
            O::SaveState => self.saved.push(self.scale),
            O::RestoreState => {
                let s = self.saved.pop().unwrap_or(self.scale);
                self.rescale(s)?;
            }
            O::SetTextFont { size, .. } => {
                let s = Scale {
                    font_size: Some(*size),
                    ..self.scale
                };
                self.rescale(s)?;
            }
            O::SetHorizontalScaling(h) => {
                let s = Scale {
                    h_scale: Some(*h),
                    ..self.scale
                };
                self.rescale(s)?;
            }
            O::SetTextLeading(l) => self.scale.leading = Some(*l),
            _ => {}
        }
        if !self.in_text {
            if matches!(op, O::BeginText) {
                self.in_text = true;
                self.ahead = 0.0;
            }
            self.out.push(op.clone());
            return Ok(());
        }
        match op {
            O::EndText => {
                self.flush(false)?;
                self.in_text = false;
                self.ahead = 0.0;
                self.out.push(op.clone());
            }
            O::MoveTextPosition(x, y) => self.line_move((*x, *y)),
            O::MoveTextPositionAndSetLeading(x, y) => {
                self.line_move((*x, *y));
                self.scale.leading = Some(-*y);
                self.run.kept.push(O::SetTextLeading(-*y));
            }
            O::NextLine => self.tstar(),
            O::SetTextMatrix(m) => {
                self.ahead = 0.0;
                self.run.absolute = Some(*m);
                self.run.line = (0.0, 0.0);
                self.run.unknown_tstar = 0;
                self.run.moved = true;
                self.run.kern = 0.0;
            }
            O::NextLineShowText(t) => {
                self.tstar();
                self.show(&[TJItem::Text(t.clone())])?;
            }
            O::SetSpacingNextLineShowText {
                word_spacing,
                char_spacing,
                text,
            } => {
                self.run.kept.push(O::SetWordSpacing(*word_spacing));
                self.run.kept.push(O::SetCharSpacing(*char_spacing));
                self.tstar();
                self.show(&[TJItem::Text(text.clone())])?;
            }
            O::ShowText(t) => self.show(&[TJItem::Text(t.clone())])?,
            O::ShowTextAdjusted(items) => self.show(items)?,
            other => self.run.kept.push(other.clone()),
        }
        Ok(())
    }

    /// A show: drawing nothing, it is advance only; else its leading
    /// and trailing numbers join the runs around it.
    fn show(&mut self, items: &[TJItem]) -> Result<(), Error> {
        let draws = |i: &TJItem| matches!(i, TJItem::Text(b) if !b.is_empty());
        let (Some(first), Some(last)) =
            (items.iter().position(draws), items.iter().rposition(draws))
        else {
            self.kerns(items);
            return Ok(());
        };
        self.kerns(&items[..first]);
        self.flush(true)?;
        let mut core: Vec<TJItem> = Vec::new();
        for i in &items[first..=last] {
            match (i, core.last_mut()) {
                (TJItem::Kern(n), Some(TJItem::Kern(m))) => *m += n,
                (TJItem::Text(b), _) if b.is_empty() => {}
                (i, _) => core.push(i.clone()),
            }
        }
        self.out.push(match core.as_slice() {
            [TJItem::Text(b)] => Operation::ShowText(b.clone()),
            _ => Operation::ShowTextAdjusted(core),
        });
        self.kerns(&items[last + 1..]);
        Ok(())
    }
}

/// Fold the positioning of removed text in `ops` (see the module
/// documentation).
pub(crate) fn fold_removed_positions(
    ops: &[Operation],
    start: TextStart,
) -> Result<Vec<Operation>, Error> {
    let mut f = Folder {
        out: Vec::with_capacity(ops.len()),
        scale: Scale {
            font_size: start.font_size,
            h_scale: start.h_scale,
            leading: start.leading,
        },
        saved: Vec::new(),
        in_text: false,
        run: Run::default(),
        ahead: 0.0,
    };
    for op in ops {
        f.op(op)?;
    }
    if f.in_text {
        // Unterminated text object: keep what does not move text.
        f.flush(false)?;
    }
    Ok(f.out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fold(src: &str, start: TextStart) -> String {
        let mut w = Vec::new();
        let ops = crate::content::parse_content_stream(
            &[src.as_bytes().to_vec()],
            &crate::limits::Limits::default(),
            false,
            &mut w,
        )
        .unwrap();
        let out = fold_removed_positions(&ops, start).unwrap();
        out.iter()
            .map(|o| o.to_string())
            .collect::<Vec<_>>()
            .join(" ")
            .replace('"', "")
    }

    #[test]
    fn removed_positions_fold_into_one_move() {
        // Per-glyph Td with the removed shows reduced to advances.
        assert_eq!(
            fold(
                "BT /F1 12 Tf 30 100 Td (Name:) Tj 45 0 Td [-600] TJ 8.1 0 Td [-600] TJ \
                 7.3 0 Td [-1800] TJ 30 0 Td (here) Tj 0 -14 Td (next) Tj ET",
                TextStart::PAGE
            ),
            "BT /F1 12 Tf 30 100 Td (Name:) Tj 90.4 0 Td (here) Tj 0 -14 Td (next) Tj ET"
        );
        // The run's trailing advance is undone before the next move.
        assert_eq!(
            fold(
                "BT /F1 10 Tf 0 0 Td (A) Tj 20 0 Td [-500] TJ (B) Tj 0 -12 Td (C) Tj ET",
                TextStart::PAGE
            ),
            "BT /F1 10 Tf 0 0 Td (A) Tj 25 0 Td (B) Tj -5 -12 Td (C) Tj ET"
        );
        // Kerns in and around a kept show: summed, folded outward.
        assert_eq!(
            fold(
                "BT /F1 10 Tf [(Name:) -2400 -20 -6600 (re) -300] TJ [-200] TJ (x) Tj ET",
                TextStart::PAGE
            ),
            "BT /F1 10 Tf [(Name:) -9020 (re)] TJ [-500] TJ (x) Tj ET"
        );
        // Absolute Tm: one Tm for the kept glyph.
        assert_eq!(
            fold(
                "BT /F1 12 Tf 1 0 0 1 30 100 Tm (Name:) Tj 1 0 0 1 75 100 Tm [-600] TJ \
                 1 0 0 1 83.1 100 Tm [-1200] TJ 1 0 0 1 180 100 Tm (here) Tj ET",
                TextStart::PAGE
            ),
            "BT /F1 12 Tf [1 0 0 1 30 100] Tm (Name:) Tj [1 0 0 1 180 100] Tm (here) Tj ET"
        );
        // Unknown leading: T* kept, counted only.
        assert_eq!(
            fold("BT (A) Tj T* [-500] TJ 3 0 Td (B) Tj ET", TextStart::FORM),
            "BT (A) Tj T* 3 0 Td (B) Tj ET"
        );
        // Nothing kept after: positions dropped; state kept.
        assert_eq!(
            fold(
                "BT /F1 9 Tf (A) Tj 4 0 Td [-500] TJ 1 Tc ET",
                TextStart::PAGE
            ),
            "BT /F1 9 Tf (A) Tj 1 Tc ET"
        );
    }
}
