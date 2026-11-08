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

use std::collections::HashSet;

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
    pub(crate) render_mode: Option<i64>,
}

impl TextStart {
    /// A page's content stream (§9.3.1: Tz 100 %, TL 0; no font yet).
    pub(crate) const PAGE: TextStart = TextStart {
        font_size: None,
        h_scale: Some(1.0),
        leading: Some(0.0),
        render_mode: Some(0),
    };
    /// A form's: everything inherited.
    pub(crate) const FORM: TextStart = TextStart {
        font_size: None,
        h_scale: None,
        leading: None,
        render_mode: None,
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

    fn tstar(&mut self) -> Result<(), Error> {
        match self.scale.leading {
            Some(l) => self.line_move((0.0, -l)),
            None => {
                // Kept as a `T*` (written before the run's other
                // operators, which may set the leading); after a `Tm`
                // it cannot be, without keeping that `Tm`.
                if self.run.absolute.is_some() {
                    return Err(unsupported("a T* of unknown leading after a Tm"));
                }
                self.line_move((0.0, 0.0));
                self.run.unknown_tstar += 1;
            }
        }
        Ok(())
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
        // `T*` moves of unknown leading come before anything that sets
        // the leading (they came before any in the run) — but not
        // before a `Q`, which may restore the leading they read.
        if before_show
            && run.unknown_tstar > 0
            && run
                .kept
                .iter()
                .any(|o| matches!(o, Operation::SaveState | Operation::RestoreState))
        {
            return Err(unsupported("a T* of unknown leading among q/Q"));
        }
        if before_show {
            for _ in 0..run.unknown_tstar {
                self.out.push(Operation::NextLine);
            }
        }
        // Values set and set again here (state for removed glyphs) are
        // dropped by `drop_dead_state` over the whole stream.
        self.out.extend(run.kept);
        if !before_show {
            return Ok(());
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
                // `Tz` is a percentage.
                let s = Scale {
                    h_scale: Some(*h / 100.0),
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
            O::NextLine => self.tstar()?,
            O::SetTextMatrix(m) => {
                self.ahead = 0.0;
                self.run.absolute = Some(*m);
                self.run.line = (0.0, 0.0);
                self.run.unknown_tstar = 0;
                self.run.moved = true;
                self.run.kern = 0.0;
            }
            O::NextLineShowText(t) => {
                self.tstar()?;
                self.show(&[TJItem::Text(t.clone())])?;
            }
            O::SetSpacingNextLineShowText {
                word_spacing,
                char_spacing,
                text,
            } => {
                self.run.kept.push(O::SetWordSpacing(*word_spacing));
                self.run.kept.push(O::SetCharSpacing(*char_spacing));
                self.tstar()?;
                self.show(&[TJItem::Text(text.clone())])?;
            }
            O::ShowText(t) => self.show(&[TJItem::Text(t.clone())])?,
            O::ShowTextAdjusted(items) => self.show(items)?,
            // What does not read the text position and is modelled:
            // text state, colour, line state, the CTM, ExtGState, paths
            // and clipping (placed by the CTM), marked content, q/Q,
            // rendering intent, flatness. Anything else inside a text
            // object (a form, an image, unknown operators) may read the
            // position or state the fold moves past it: refused.
            O::SetCharSpacing(_)
            | O::SetWordSpacing(_)
            | O::SetHorizontalScaling(_)
            | O::SetTextLeading(_)
            | O::SetTextRise(_)
            | O::SetTextRenderingMode(_)
            | O::SetTextFont { .. }
            | O::SetGrayFill(_)
            | O::SetRgbFill(..)
            | O::SetCmykFill(..)
            | O::SetColorSpaceFill(_)
            | O::SetColorFill(_)
            | O::SetGrayStroke(_)
            | O::SetRgbStroke(..)
            | O::SetCmykStroke(..)
            | O::SetColorSpaceStroke(_)
            | O::SetColorStroke(_)
            | O::BeginMarkedContent(_)
            | O::BeginMarkedContentWithProps { .. }
            | O::EndMarkedContent
            | O::MarkedContentPoint(_)
            | O::DesignatedMarkedContentPoint { .. }
            | O::SaveState
            | O::RestoreState
            | O::SetRenderingIntent(_)
            | O::SetFlatness(_)
            | O::SetExtGState(_)
            | O::ConcatMatrix(_)
            | O::SetLineWidth(_)
            | O::SetLineCap(_)
            | O::SetLineJoin(_)
            | O::SetMiterLimit(_)
            | O::SetDash { .. }
            | O::MoveTo(..)
            | O::LineTo(..)
            | O::CurveTo(..)
            | O::CurveToV(..)
            | O::CurveToY(..)
            | O::ClosePath
            | O::Rectangle(..)
            | O::EndPath
            | O::Clip
            | O::ClipEvenOdd
            | O::Stroke
            | O::CloseAndStroke
            | O::Fill
            | O::FillEvenOdd
            | O::FillAndStroke
            | O::FillAndStrokeEvenOdd
            | O::CloseFillAndStroke
            | O::CloseFillAndStrokeEvenOdd => self.run.kept.push(op.clone()),
            other => {
                return Err(unsupported(&format!(
                    "`{}` inside a text object among removed text",
                    other.name()
                )))
            }
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
/// `gs_keys` gives the keys of an ExtGState resource (`None`: not
/// known).
pub(crate) fn fold_removed_positions(
    ops: &[Operation],
    start: TextStart,
    gs_keys: GsKeys,
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
    let ops = drop_dead_state(f.out, start, gs_keys);
    Ok(drop_empty_pairs(compose_cm(ops)))
}

/// The keys of an ExtGState resource, by name (`None`: not known).
pub(crate) type GsKeys<'a> = &'a dyn Fn(&[u8]) -> Option<Vec<Vec<u8>>>;

/// Slots of the graphics state a store writes and a reader reads.
const SLOTS: usize = 17;
const TL: usize = 3;
const TR: usize = 5;
const FONT: usize = 6;
const FILL_VALUE: usize = 7;
const FILL_SPACE: usize = 8;
const STROKE_VALUE: usize = 9;
const STROKE_SPACE: usize = 10;
/// Line width, cap, join, miter limit, dash.
const LINE: [usize; 5] = [11, 12, 13, 14, 15];
/// Something paints later in the block: reads the path, the clip, the
/// CTM, rendering intent and flatness.
const PAINT: usize = 16;

/// ExtGState keys written later and not read since, per block.
#[derive(Clone, Default)]
struct GsLive {
    /// Every key is live but `except`; else only `some`.
    all: bool,
    except: HashSet<Vec<u8>>,
    some: HashSet<Vec<u8>>,
}

impl GsLive {
    fn read_all(&mut self) {
        *self = GsLive {
            all: true,
            ..GsLive::default()
        };
    }
    fn is_live(&self, k: &[u8]) -> bool {
        if self.all {
            !self.except.contains(k)
        } else {
            self.some.contains(k)
        }
    }
    fn kill(&mut self, k: &[u8]) {
        if self.all {
            self.except.insert(k.to_vec());
        } else {
            self.some.remove(k);
        }
    }
    fn union(self, o: GsLive) -> GsLive {
        match (self.all, o.all) {
            (true, true) => GsLive {
                all: true,
                except: self.except.intersection(&o.except).cloned().collect(),
                some: HashSet::new(),
            },
            (true, false) | (false, true) => {
                let (a, b) = if self.all { (self, o) } else { (o, self) };
                GsLive {
                    all: true,
                    except: a.except.difference(&b.some).cloned().collect(),
                    some: HashSet::new(),
                }
            }
            (false, false) => GsLive {
                all: false,
                except: HashSet::new(),
                some: self.some.union(&o.some).cloned().collect(),
            },
        }
    }
}

/// The ExtGState keys that write a slot modelled here.
fn gs_slot(k: &[u8]) -> Option<usize> {
    Some(match k {
        b"LW" => LINE[0],
        b"LC" => LINE[1],
        b"LJ" => LINE[2],
        b"ML" => LINE[3],
        b"D" => LINE[4],
        b"Font" => FONT,
        b"RI" | b"FL" => PAINT,
        _ => return None,
    })
}

/// `ops` without the stores nothing reads: a text-state parameter, a
/// colour, line state, an ExtGState key, a path or clip, the CTM set
/// and set again (or left unread at the end of the stream, or before a
/// `Q` restoring it) before anything draws with it. The leading a `TD`
/// folded into a `Td` and a `TL` set to its line's offset — measured
/// from a removed line, as often as not — goes this way, and so do the
/// styling of a text object whose glyphs were all removed and the
/// clipping around it. Readers: a show reads the text state, the fill
/// colour and — in a stroking rendering mode (or one not known here) —
/// the stroke colour and line state; a fill reads the fill colour, a
/// stroke the stroke colour and line state; `T*`, `'` and `"` the
/// leading; anything painting reads every ExtGState key and the path,
/// clip and CTM. `gs_keys` gives the keys an ExtGState sets (`None`:
/// not known — then it reads everything). Anything not modelled (a
/// form, an image, a shading, unknown operators) reads everything.
fn drop_dead_state(ops: Vec<Operation>, start: TextStart, gs_keys: GsKeys) -> Vec<Operation> {
    use Operation as O;
    // The rendering mode at each show, forward (saved with q/Q).
    let mut stroking = vec![true; ops.len()];
    {
        let mut tr = start.render_mode;
        let mut saved = Vec::new();
        for (i, o) in ops.iter().enumerate() {
            match o {
                O::SaveState => saved.push(tr),
                O::RestoreState => tr = saved.pop().unwrap_or(None),
                O::SetTextRenderingMode(m) => tr = Some(*m),
                _ => {}
            }
            stroking[i] = !matches!(tr, Some(0 | 3 | 4 | 7));
        }
    }
    let all = [true; SLOTS];
    let mut live = [false; SLOTS];
    let mut gs = GsLive::default();
    let mut blocks: Vec<([bool; SLOTS], GsLive)> = Vec::new();
    let mut keep = vec![true; ops.len()];
    let fill = |live: &mut [bool; SLOTS]| {
        live[FILL_VALUE] = true;
        live[FILL_SPACE] = true;
    };
    let stroke = |live: &mut [bool; SLOTS]| {
        live[STROKE_VALUE] = true;
        live[STROKE_SPACE] = true;
        for s in LINE {
            live[s] = true;
        }
    };
    let store = |live: &mut [bool; SLOTS], slot: usize| std::mem::replace(&mut live[slot], false);
    for (i, o) in ops.iter().enumerate().rev() {
        keep[i] = match o {
            O::SetCharSpacing(_) => store(&mut live, 0),
            O::SetWordSpacing(_) => store(&mut live, 1),
            O::SetHorizontalScaling(_) => store(&mut live, 2),
            O::SetTextLeading(_) => store(&mut live, TL),
            O::SetTextRise(_) => store(&mut live, 4),
            O::SetTextRenderingMode(_) => store(&mut live, TR),
            O::SetTextFont { .. } => store(&mut live, FONT),
            O::SetLineWidth(_) => store(&mut live, LINE[0]),
            O::SetLineCap(_) => store(&mut live, LINE[1]),
            O::SetLineJoin(_) => store(&mut live, LINE[2]),
            O::SetMiterLimit(_) => store(&mut live, LINE[3]),
            O::SetDash { .. } => store(&mut live, LINE[4]),
            O::SetColorFill(_) | O::SetColorStroke(_) => {
                let (v, sp) = if matches!(o, O::SetColorFill(_)) {
                    (FILL_VALUE, FILL_SPACE)
                } else {
                    (STROKE_VALUE, STROKE_SPACE)
                };
                let k = store(&mut live, v);
                if k {
                    live[sp] = true;
                }
                k
            }
            O::SetGrayFill(_)
            | O::SetRgbFill(..)
            | O::SetCmykFill(..)
            | O::SetColorSpaceFill(_) => {
                let k = live[FILL_VALUE] || live[FILL_SPACE];
                live[FILL_VALUE] = false;
                live[FILL_SPACE] = false;
                k
            }
            O::SetGrayStroke(_)
            | O::SetRgbStroke(..)
            | O::SetCmykStroke(..)
            | O::SetColorSpaceStroke(_) => {
                let k = live[STROKE_VALUE] || live[STROKE_SPACE];
                live[STROKE_VALUE] = false;
                live[STROKE_SPACE] = false;
                k
            }
            O::SetExtGState(name) => match gs_keys(name) {
                None => {
                    live = all;
                    gs.read_all();
                    true
                }
                Some(keys) => {
                    let k = keys.iter().any(|key| match gs_slot(key) {
                        Some(s) => live[s],
                        None => gs.is_live(key),
                    });
                    for key in &keys {
                        match gs_slot(key) {
                            Some(s) if s != PAINT => live[s] = false,
                            Some(_) => {}
                            None => gs.kill(key),
                        }
                    }
                    k
                }
            },
            O::ShowText(b) | O::NextLineShowText(b) if b.is_empty() => {
                if matches!(o, O::NextLineShowText(_)) {
                    live[TL] = true;
                }
                true
            }
            O::ShowText(_)
            | O::ShowTextAdjusted(_)
            | O::NextLineShowText(_)
            | O::SetSpacingNextLineShowText { .. } => {
                for s in [0, 1, 2, 4, TR, FONT] {
                    live[s] = true;
                }
                if !matches!(o, O::ShowText(_) | O::ShowTextAdjusted(_)) {
                    live[TL] = true;
                }
                fill(&mut live);
                if stroking[i] {
                    stroke(&mut live);
                }
                live[PAINT] = true;
                gs.read_all();
                true
            }
            O::NextLine => {
                live[TL] = true;
                true
            }
            O::Fill | O::FillEvenOdd => {
                fill(&mut live);
                live[PAINT] = true;
                gs.read_all();
                true
            }
            O::Stroke | O::CloseAndStroke => {
                stroke(&mut live);
                live[PAINT] = true;
                gs.read_all();
                true
            }
            O::FillAndStroke
            | O::FillAndStrokeEvenOdd
            | O::CloseFillAndStroke
            | O::CloseFillAndStrokeEvenOdd => {
                fill(&mut live);
                stroke(&mut live);
                live[PAINT] = true;
                gs.read_all();
                true
            }
            O::MoveTo(..)
            | O::LineTo(..)
            | O::CurveTo(..)
            | O::CurveToV(..)
            | O::CurveToY(..)
            | O::ClosePath
            | O::Rectangle(..)
            | O::EndPath
            | O::Clip
            | O::ClipEvenOdd
            | O::ConcatMatrix(_)
            | O::SetRenderingIntent(_)
            | O::SetFlatness(_) => live[PAINT],
            O::RestoreState => {
                blocks.push((live, std::mem::take(&mut gs)));
                live = [false; SLOTS];
                true
            }
            O::SaveState => {
                // Before the block, a value is read by the block (as
                // walked) or by what follows its `Q`.
                let (after, after_gs) = blocks.pop().unwrap_or((
                    all,
                    GsLive {
                        all: true,
                        ..GsLive::default()
                    },
                ));
                for (l, a) in live.iter_mut().zip(after) {
                    *l |= a;
                }
                gs = std::mem::take(&mut gs).union(after_gs);
                true
            }
            O::BeginText
            | O::EndText
            | O::MoveTextPosition(..)
            | O::SetTextMatrix(_)
            | O::BeginMarkedContent(_)
            | O::BeginMarkedContentWithProps { .. }
            | O::EndMarkedContent
            | O::MarkedContentPoint(_)
            | O::DesignatedMarkedContentPoint { .. } => true,
            O::MoveTextPositionAndSetLeading(..) => {
                live[TL] = false;
                true
            }
            _ => {
                live = all;
                gs.read_all();
                true
            }
        };
    }
    ops.into_iter()
        .zip(keep)
        .filter_map(|(o, k)| k.then_some(o))
        .collect()
}

/// `ops` with consecutive `cm` composed — those with nothing reading the
/// CTM between them (only colour, text state and positioning, marked
/// content …) — and dropped where they compose to the identity (to
/// 1e-12): a translation set for removed text and undone after it does
/// not stay.
fn compose_cm(ops: Vec<Operation>) -> Vec<Operation> {
    use Operation as O;
    let mut out: Vec<Option<Operation>> = Vec::with_capacity(ops.len());
    // The last `cm` with nothing reading the CTM since.
    let mut pending: Option<usize> = None;
    let identity = |m: &Matrix| {
        [m.a - 1.0, m.b, m.c, m.d - 1.0, m.e, m.f]
            .iter()
            .all(|v| v.abs() <= 1e-12)
    };
    for o in ops {
        match o {
            O::ConcatMatrix(m) => {
                let m = match pending.and_then(|p| out[p].take()) {
                    Some(O::ConcatMatrix(prev)) => m.concat(prev),
                    _ => m,
                };
                if identity(&m) {
                    pending = None;
                } else {
                    out.push(Some(O::ConcatMatrix(m)));
                    pending = Some(out.len() - 1);
                }
            }
            O::SetCharSpacing(_)
            | O::SetWordSpacing(_)
            | O::SetHorizontalScaling(_)
            | O::SetTextLeading(_)
            | O::SetTextRise(_)
            | O::SetTextRenderingMode(_)
            | O::SetTextFont { .. }
            | O::SetGrayFill(_)
            | O::SetRgbFill(..)
            | O::SetCmykFill(..)
            | O::SetColorSpaceFill(_)
            | O::SetColorFill(_)
            | O::SetGrayStroke(_)
            | O::SetRgbStroke(..)
            | O::SetCmykStroke(..)
            | O::SetColorSpaceStroke(_)
            | O::SetColorStroke(_)
            | O::BeginText
            | O::EndText
            | O::MoveTextPosition(..)
            | O::MoveTextPositionAndSetLeading(..)
            | O::SetTextMatrix(_)
            | O::NextLine
            | O::BeginMarkedContent(_)
            | O::BeginMarkedContentWithProps { .. }
            | O::EndMarkedContent
            | O::MarkedContentPoint(_)
            | O::DesignatedMarkedContentPoint { .. }
            | O::SetLineWidth(_)
            | O::SetLineCap(_)
            | O::SetLineJoin(_)
            | O::SetMiterLimit(_)
            | O::SetDash { .. }
            | O::SetRenderingIntent(_)
            | O::SetFlatness(_) => out.push(Some(o)),
            O::ShowText(ref b) | O::NextLineShowText(ref b) if b.is_empty() => out.push(Some(o)),
            other => {
                pending = None;
                out.push(Some(other));
            }
        }
    }
    out.into_iter().flatten().collect()
}

/// `ops` without `BT ET` and `q Q` pairs holding nothing (what removed
/// text leaves when it was all of a text object or a saved state) —
/// they draw nothing and change no state after them.
fn drop_empty_pairs(mut ops: Vec<Operation>) -> Vec<Operation> {
    use Operation as O;
    loop {
        let mut out: Vec<Operation> = Vec::with_capacity(ops.len());
        let mut changed = false;
        for o in ops {
            match (out.last(), &o) {
                (Some(O::BeginText), O::EndText) | (Some(O::SaveState), O::RestoreState) => {
                    out.pop();
                    changed = true;
                }
                _ => out.push(o),
            }
        }
        ops = out;
        if !changed {
            return ops;
        }
    }
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
        let out = fold_removed_positions(&ops, start, &|_| None).unwrap();
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
            // (the Tc after is read by nothing: dropped)
            "BT /F1 9 Tf (A) Tj ET"
        );
    }

    #[test]
    fn review_cases() {
        // Tz is a percentage: 100 Tz leaves the advance as it is.
        assert_eq!(
            fold(
                "BT /F1 10 Tf 100 Tz 0 0 Td (A) Tj 7 0 Td [-500] TJ (B) Tj ET",
                TextStart::FORM
            ),
            "BT /F1 10 Tf 100 Tz 0 0 Td (A) Tj 12 0 Td (B) Tj ET"
        );
        // A T* of unknown leading comes before the run's TL (here
        // read by nothing after, so dropped).
        assert_eq!(
            fold(
                "BT /F1 10 Tf 100 Tz (A) Tj T* 12 TL [-500] TJ (B) Tj ET",
                TextStart::FORM
            ),
            "BT /F1 10 Tf 100 Tz (A) Tj T* 5 0 Td (B) Tj ET"
        );
        // ... and cannot follow a Tm.
        let mut w = Vec::new();
        let ops = crate::content::parse_content_stream(
            &[b"BT /F1 10 Tf (A) Tj 1 0 0 1 5 5 Tm T* (B) Tj ET".to_vec()],
            &crate::limits::Limits::default(),
            false,
            &mut w,
        )
        .unwrap();
        assert!(fold_removed_positions(&ops, TextStart::FORM, &|_| None).is_err());
        // The leading each removed line's TD set is not kept; the last
        // is kept only where something reads it.
        assert_eq!(
            fold(
                "BT /F1 10 Tf 0 0 Td (A) Tj 0 -10 TD [-500] TJ 0 -20 TD [-500] TJ 0 -12 TD (C) Tj ET",
                TextStart::PAGE
            ),
            "BT /F1 10 Tf 0 0 Td (A) Tj 0 -42 Td (C) Tj ET"
        );
        assert_eq!(
            fold(
                "BT /F1 10 Tf 0 0 Td (A) Tj 0 -10 TD [-500] TJ 0 -12 TD (C) Tj T* (D) Tj ET",
                TextStart::PAGE
            ),
            // The T* of known leading folds too, so nothing reads it.
            "BT /F1 10 Tf 0 0 Td (A) Tj 0 -22 Td (C) Tj 0 -12 Td (D) Tj ET"
        );
        // A T* of unknown leading after a Q (which restores the
        // leading it reads): refused, not reordered.
        let mut w = Vec::new();
        let ops = crate::content::parse_content_stream(
            &[
                b"BT /F 10 Tf 100 Tz q 30 TL 50 100 Td (A) Tj 100 0 Td [-500] TJ Q T* (C) Tj ET"
                    .to_vec(),
            ],
            &crate::limits::Limits::default(),
            false,
            &mut w,
        )
        .unwrap();
        assert!(fold_removed_positions(&ops, TextStart::FORM, &|_| None).is_err());
        // Colours set for removed text (a link colour per removed
        // citation) collapse to the one in force for the kept text.
        assert_eq!(
            fold(
                "BT /F1 10 Tf (A) Tj 0 0 1 rg 0 0 1 RG [-500] TJ 0 g 0 G [-300] TJ 0 0 1 rg [-200] TJ \
                 0 g (B) Tj ET",
                TextStart::PAGE
            ),
            // (the stroke colour, unread by fill-only text, goes too)
            "BT /F1 10 Tf (A) Tj 0 g [-1000] TJ (B) Tj ET"
        );
        // A form inside a text object reads what the fold moves past
        // it: refused.
        let mut w = Vec::new();
        let ops = crate::content::parse_content_stream(
            &[b"BT /F1 10 Tf (A) Tj 5 0 Td [-500] TJ /Fm Do (B) Tj ET".to_vec()],
            &crate::limits::Limits::default(),
            false,
            &mut w,
        )
        .unwrap();
        assert!(fold_removed_positions(&ops, TextStart::PAGE, &|_| None).is_err());
        // The styling of a text object left drawing nothing is dropped,
        // and so are the empty pairs it leaves.
        for ts in ["1", "2"] {
            assert_eq!(
                fold(
                    &format!(
                        "q BT /F1 10 Tf {ts} Ts 0 0 1 rg 5 5 Td [-500] TJ ET Q \
                         BT /F1 10 Tf 0 Ts 0 g 9 9 Td (C) Tj ET"
                    ),
                    TextStart::PAGE
                ),
                "BT /F1 10 Tf 0 Ts 0 g 9 9 Td (C) Tj ET"
            );
        }
        // A value set before q is read inside the block.
        assert_eq!(
            fold("/F1 10 Tf 0 0 1 rg q BT (A) Tj ET Q", TextStart::PAGE),
            "/F1 10 Tf 0 0 1 rg q BT (A) Tj ET Q"
        );
        // A path painted inside the run reads the colour before it.
        assert_eq!(
            fold(
                "BT /F1 10 Tf (A) Tj 1 0 0 rg 0 0 5 5 re f 0 g [-500] TJ (B) Tj ET",
                TextStart::PAGE
            ),
            "BT /F1 10 Tf (A) Tj 1 0 0 rg 0 0 5 5 re f 0 g [-500] TJ (B) Tj ET"
        );
    }
}
