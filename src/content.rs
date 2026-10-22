//! Content stream parser → typed operations (M2).
//!
//! Parses the postfix-notation command stream of a page (or Form
//! XObject) into a [`Vec<Operation>`]. Operands are collected until an
//! operator token appears; the operator is then matched against a
//! table of known operators with operand count and type checks.
//!
//! Spec: ISO 32000-1:2008, §7.8 (content streams) and operator tables
//! in §8–§9. Concatenated `/Contents` arrays are treated as one
//! logical stream: "the effect shall be as if the stream objects were
//! concatenated" (§7.8.1). Therefore operand stacks and structural
//! state are *not* reset between elements.
//!
//! Unlike the object-file parser, this module only accepts *direct*
//! objects as operands — `n g R` references, `obj`/`endobj` bodies and
//! `stream` data cannot legally appear in a content stream and are
//! rejected (e.g. `10 0 R` is an unknown operator `R` error).
//!
//! Unsupported or malformed input aborts the whole parse — partial
//! results are never returned:
//!
//! * Unknown operator outside `BX`/`EX` → `Error::Parse`
//! * `BI`/`ID`/`EI` (inline images) → `Error::Unsupported`
//! * Wrong operand count or type → `Error::Parse`
//! * Leftover operands at end of input → `Error::Parse`
//! * Unbalanced `q`/`Q`, `BT`/`ET`, `BX`/`EX`, `BMC`/`BDC`/`EMC` →
//!   `Error::Structure`
//!
//! Inside a `BX … EX` compatibility section unknown operators are
//! recorded as [`Operation::Unknown`] rather than failing (§7.8.2).

use crate::error::Error;
use crate::lexer::{Lexer, Token};
use crate::limits::Limits;
use crate::matrix::Matrix;
use crate::object::{Dict, Name, Object};
use std::fmt;

/// One element of a `TJ` array.
#[derive(Debug, Clone, PartialEq)]
pub enum TJItem {
    /// String bytes (literal or hex string, raw bytes — not UTF-8).
    Text(Vec<u8>),
    /// Kerning adjustment (thousandths of a text space unit).
    Kern(f64),
}

/// One parsed content-stream operation with typed operands.
///
/// Resource-referencing operands (`Tf`, `Do`, `gs`, `CS`, `cs`, `sh`,
/// `BDC`/`DP` property names) are kept as `Name`s; they can be
/// resolved through `Document::page_resource` when needed.
#[derive(Debug, Clone, PartialEq)]
pub enum Operation {
    // -- Graphics state & matrix (§8.4) ------------------------------
    /// `q` — push graphics state.
    SaveState,
    /// `Q` — pop graphics state.
    RestoreState,
    /// `cm` — concatenate matrix to the CTM.
    ConcatMatrix(Matrix),
    /// `w` — set line width.
    SetLineWidth(f64),
    /// `J` — set line cap style (0–2).
    SetLineCap(i64),
    /// `j` — set line join style (0–2).
    SetLineJoin(i64),
    /// `M` — set miter limit.
    SetMiterLimit(f64),
    /// `d` — set dash pattern `[array] phase`.
    SetDash {
        /// Dash lengths (on/off alternating).
        dashes: Vec<f64>,
        /// Phase offset into the pattern.
        phase: f64,
    },
    /// `ri` — set rendering intent.
    SetRenderingIntent(Name),
    /// `i` — set flatness tolerance.
    SetFlatness(f64),
    /// `gs` — set graphics state parameters from an ExtGState resource.
    SetExtGState(Name),

    // -- Path construction & painting (§8.5) -------------------------
    /// `m` — move to point.
    MoveTo(f64, f64),
    /// `l` — line to point.
    LineTo(f64, f64),
    /// `c` — curve to (three control points).
    CurveTo(f64, f64, f64, f64, f64, f64),
    /// `v` — curve to (first control point equals current point).
    CurveToV(f64, f64, f64, f64),
    /// `y` — curve to (last control point equals end point).
    CurveToY(f64, f64, f64, f64),
    /// `h` — close subpath.
    ClosePath,
    /// `re` — rectangle.
    Rectangle(f64, f64, f64, f64),
    /// `S` — stroke.
    Stroke,
    /// `s` — close and stroke.
    CloseAndStroke,
    /// `f`/`F` — fill (nonzero winding).
    Fill,
    /// `f*` — fill (even-odd).
    FillEvenOdd,
    /// `B` — fill and stroke.
    FillAndStroke,
    /// `B*` — fill and stroke (even-odd).
    FillAndStrokeEvenOdd,
    /// `b` — close, fill and stroke.
    CloseFillAndStroke,
    /// `b*` — close, fill and stroke (even-odd).
    CloseFillAndStrokeEvenOdd,
    /// `n` — end path without painting.
    EndPath,
    /// `W` — set clipping path (nonzero winding).
    Clip,
    /// `W*` — set clipping path (even-odd).
    ClipEvenOdd,

    // -- Colour (§8.6) -----------------------------------------------
    /// `G` — set stroking colour (DeviceGray).
    SetGrayStroke(f64),
    /// `g` — set non-stroking colour (DeviceGray).
    SetGrayFill(f64),
    /// `RG` — set stroking colour (DeviceRGB).
    SetRgbStroke(f64, f64, f64),
    /// `rg` — set non-stroking colour (DeviceRGB).
    SetRgbFill(f64, f64, f64),
    /// `K` — set stroking colour (DeviceCMYK).
    SetCmykStroke(f64, f64, f64, f64),
    /// `k` — set non-stroking colour (DeviceCMYK).
    SetCmykFill(f64, f64, f64, f64),
    /// `CS` — set stroking colour space (name).
    SetColorSpaceStroke(Name),
    /// `cs` — set non-stroking colour space (name).
    SetColorSpaceFill(Name),
    /// `SC`/`SCN` — stroking colour components. The operand count
    /// depends on the current colour space; for `SCN` the last operand
    /// may be a pattern name. Semantics need the colour space, so the
    /// raw operands are kept.
    SetColorStroke(Vec<Object>),
    /// `sc`/`scn` — non-stroking colour components.
    SetColorFill(Vec<Object>),

    // -- Text (§9) ----------------------------------------------------
    /// `BT` — begin text object.
    BeginText,
    /// `ET` — end text object.
    EndText,
    /// `Tc` — set character spacing.
    SetCharSpacing(f64),
    /// `Tw` — set word spacing.
    SetWordSpacing(f64),
    /// `Tz` — set horizontal scaling (percent ÷ 100).
    SetHorizontalScaling(f64),
    /// `TL` — set text leading.
    SetTextLeading(f64),
    /// `Tf` — set text font and size (`font` is a resource name).
    SetTextFont {
        /// Font resource name (resolve via `Document::page_resource`).
        font: Name,
        /// Font size in text space units.
        size: f64,
    },
    /// `Tr` — set text rendering mode (0–7).
    SetTextRenderingMode(i64),
    /// `Ts` — set text rise.
    SetTextRise(f64),
    /// `Td` — move text position.
    MoveTextPosition(f64, f64),
    /// `TD` — move text position and set leading.
    MoveTextPositionAndSetLeading(f64, f64),
    /// `Tm` — set text matrix and text line matrix.
    SetTextMatrix(Matrix),
    /// `T*` — move to start of next line.
    NextLine,
    /// `Tj` — show text (raw bytes; not decoded to Unicode).
    ShowText(Vec<u8>),
    /// `TJ` — show text with individual glyph positioning.
    ShowTextAdjusted(Vec<TJItem>),
    /// `'` — move to next line and show text.
    NextLineShowText(Vec<u8>),
    /// `"` — set spacing, move to next line, show text.
    SetSpacingNextLineShowText {
        /// `Tw` value.
        word_spacing: f64,
        /// `Tc` value.
        char_spacing: f64,
        /// Text bytes.
        text: Vec<u8>,
    },

    // -- Type 3 glyph programs (§9.6.4) ------------------------------
    // `d0`/`d1` only appear inside a Type3 `/CharProcs` stream — they
    // declare the glyph's metrics before the drawing ops. Outside a
    // CharProc they are meaningless; the parser keeps them as data and
    // the Type3 executor interprets them.
    /// `d0` — set glyph width only (coloured glyph): `wx wy d0`.
    Type3SetWidth {
        /// Horizontal advance in glyph space.
        wx: f64,
        /// Vertical advance in glyph space.
        wy: f64,
    },
    /// `d1` — set width + glyph bounding box (uncoloured glyph):
    /// `wx wy llx lly urx ury d1`.
    Type3SetWidthAndBox {
        /// Horizontal advance in glyph space.
        wx: f64,
        /// Vertical advance in glyph space.
        wy: f64,
        /// Glyph bbox lower-left x.
        llx: f64,
        /// Glyph bbox lower-left y.
        lly: f64,
        /// Glyph bbox upper-right x.
        urx: f64,
        /// Glyph bbox upper-right y.
        ury: f64,
    },

    // -- External objects & marked content (§8.8, §8.10, §10.5) ------
    /// `Do` — invoke a named XObject.
    Do(Name),
    /// `sh` — paint a shading pattern.
    Shading(Name),
    /// `MP` — marked-content point.
    MarkedContentPoint(Name),
    /// `DP` — marked-content point with properties.
    DesignatedMarkedContentPoint {
        /// Tag name.
        tag: Name,
        /// Properties: dictionary or resource name.
        properties: Object,
    },
    /// `BMC` — begin marked-content sequence.
    BeginMarkedContent(Name),
    /// `BDC` — begin marked-content sequence with properties.
    BeginMarkedContentWithProps {
        /// Tag name.
        tag: Name,
        /// Properties: dictionary or resource name.
        properties: Object,
    },
    /// `EMC` — end marked-content sequence.
    EndMarkedContent,
    /// `BX` — begin compatibility section.
    BeginCompatibility,
    /// `EX` — end compatibility section.
    EndCompatibility,

    /// Inside `BX … EX`, an operator we do not implement is recorded
    /// instead of failing (§7.8.2 allows readers to ignore it).
    Unknown {
        /// The operator token.
        operator: Vec<u8>,
        /// The operands it carried.
        operands: Vec<Object>,
    },
}

impl Operation {
    /// Whether this operator may legally appear *inside* a path object
    /// — i.e. between a path-construction op and the painting/`n` op
    /// that terminates it (§8.2 Figure 9, §8.5.4). Only
    /// path-construction ops, `W`/`W*`, and the painting ops are
    /// allowed there; everything else is out of order.
    ///
    /// This is a *placement* predicate, not a parsing one — the lexer
    /// still accepts the full grammar; `render_page` uses this to
    /// refuse out-of-order operator sequences with a typed error.
    pub fn is_path_level(&self) -> bool {
        matches!(
            self,
            Operation::MoveTo(..)
                | Operation::LineTo(..)
                | Operation::CurveTo(..)
                | Operation::CurveToV(..)
                | Operation::CurveToY(..)
                | Operation::ClosePath
                | Operation::Rectangle(..)
                | Operation::Stroke
                | Operation::CloseAndStroke
                | Operation::Fill
                | Operation::FillEvenOdd
                | Operation::FillAndStroke
                | Operation::FillAndStrokeEvenOdd
                | Operation::CloseFillAndStroke
                | Operation::CloseFillAndStrokeEvenOdd
                | Operation::EndPath
                | Operation::Clip
                | Operation::ClipEvenOdd
        )
    }

    /// The PDF operator name for diagnostics.
    pub fn name(&self) -> &'static str {
        match self {
            Operation::SaveState => "q",
            Operation::RestoreState => "Q",
            Operation::ConcatMatrix(_) => "cm",
            Operation::SetLineWidth(_) => "w",
            Operation::SetLineCap(_) => "J",
            Operation::SetLineJoin(_) => "j",
            Operation::SetMiterLimit(_) => "M",
            Operation::SetDash { .. } => "d",
            Operation::SetRenderingIntent(_) => "ri",
            Operation::SetFlatness(_) => "i",
            Operation::SetExtGState(_) => "gs",
            Operation::MoveTo(..) => "m",
            Operation::LineTo(..) => "l",
            Operation::CurveTo(..) => "c",
            Operation::CurveToV(..) => "v",
            Operation::CurveToY(..) => "y",
            Operation::ClosePath => "h",
            Operation::Rectangle(..) => "re",
            Operation::Stroke => "S",
            Operation::CloseAndStroke => "s",
            Operation::Fill => "f",
            Operation::FillEvenOdd => "f*",
            Operation::FillAndStroke => "B",
            Operation::FillAndStrokeEvenOdd => "B*",
            Operation::CloseFillAndStroke => "b",
            Operation::CloseFillAndStrokeEvenOdd => "b*",
            Operation::EndPath => "n",
            Operation::Clip => "W",
            Operation::ClipEvenOdd => "W*",
            Operation::SetGrayStroke(_) => "G",
            Operation::SetGrayFill(_) => "g",
            Operation::SetRgbStroke(..) => "RG",
            Operation::SetRgbFill(..) => "rg",
            Operation::SetCmykStroke(..) => "K",
            Operation::SetCmykFill(..) => "k",
            Operation::SetColorSpaceStroke(_) => "CS",
            Operation::SetColorSpaceFill(_) => "cs",
            Operation::SetColorStroke(_) => "SC/SCN",
            Operation::SetColorFill(_) => "sc/scn",
            Operation::BeginText => "BT",
            Operation::EndText => "ET",
            Operation::SetCharSpacing(_) => "Tc",
            Operation::SetWordSpacing(_) => "Tw",
            Operation::SetHorizontalScaling(_) => "Tz",
            Operation::SetTextLeading(_) => "TL",
            Operation::SetTextFont { .. } => "Tf",
            Operation::SetTextRenderingMode(_) => "Tr",
            Operation::SetTextRise(_) => "Ts",
            Operation::MoveTextPosition(..) => "Td",
            Operation::MoveTextPositionAndSetLeading(..) => "TD",
            Operation::SetTextMatrix(_) => "Tm",
            Operation::NextLine => "T*",
            Operation::ShowText(_) => "Tj",
            Operation::ShowTextAdjusted(_) => "TJ",
            Operation::NextLineShowText(_) => "'",
            Operation::SetSpacingNextLineShowText { .. } => "\"",
            Operation::Type3SetWidth { .. } => "d0",
            Operation::Type3SetWidthAndBox { .. } => "d1",
            Operation::Do(_) => "Do",
            Operation::Shading(_) => "sh",
            Operation::MarkedContentPoint(_) => "MP",
            Operation::DesignatedMarkedContentPoint { .. } => "DP",
            Operation::BeginMarkedContent(_) => "BMC",
            Operation::BeginMarkedContentWithProps { .. } => "BDC",
            Operation::EndMarkedContent => "EMC",
            Operation::BeginCompatibility => "BX",
            Operation::EndCompatibility => "EX",
            Operation::Unknown { .. } => "unknown",
        }
    }
}

/// Parse the concatenated `/Contents` streams into operations.
///
/// `contents` is the decoded stream bodies in order (as returned by
/// `Document::page_contents`). The streams are treated as one logical
/// sequence: operand stacks and structural state (text blocks,
/// marked-content depth, `q`/`Q` depth, `BX`/`EX`) carry over across
/// boundaries.
pub(crate) fn parse_content_stream(
    contents: &[Vec<u8>],
    limits: &Limits,
) -> Result<Vec<Operation>, Error> {
    let (ops, _tokens) = parse_content_stream_counted(contents, limits)?;
    Ok(ops)
}

/// Like [`parse_content_stream`] but also returns the total number of
/// *lexer tokens* consumed across all streams — operands *and*
/// operators, so `tokens >= ops`. Used by `inspect` to report the
/// raw token count separately from the operation count.
pub(crate) fn parse_content_stream_counted(
    contents: &[Vec<u8>],
    limits: &Limits,
) -> Result<(Vec<Operation>, usize), Error> {
    let mut parser = ContentParser::new(limits);
    for stream in contents {
        parser.parse_stream(stream)?;
    }
    let tokens = parser.tokens_seen;
    let ops = parser.finish()?;
    Ok((ops, tokens))
}

/// The three results `inspect` needs from a content-stream parse:
/// the operations, the raw operator token behind each, and the
/// total lexer-token count.
pub(crate) type NamedOps = (Vec<Operation>, Vec<Vec<u8>>, usize);

/// Like [`parse_content_stream_counted`] but also returns the exact
/// operator token for every parsed `Operation` — the bytes as they
/// appear in the stream (`SC` vs `SCN`, `sc` vs `scn`, or the real
/// name behind `Unknown`). Used by `inspect`'s `opstats`, which must
/// report spec operator names, not grouped display names.
pub(crate) fn parse_content_stream_named(
    contents: &[Vec<u8>],
    limits: &Limits,
) -> Result<NamedOps, Error> {
    let mut parser = ContentParser::new(limits);
    for stream in contents {
        parser.parse_stream(stream)?;
    }
    let tokens = parser.tokens_seen;
    let names = parser.op_names.clone();
    let ops = parser.finish()?;
    Ok((ops, names, tokens))
}

struct ContentParser<'a> {
    limits: &'a Limits,
    operands: Vec<Object>,
    ops: Vec<Operation>,
    /// Total lexer tokens consumed across all parsed streams —
    /// operands and operators combined. `inspect` reports this as
    /// the raw token count.
    tokens_seen: usize,
    /// The exact operator bytes behind every entry in `ops`, parallel
    /// to it — `SC` vs `SCN`, `sc` vs `scn`, or the real name an
    /// `Unknown` op parsed. `inspect` reports these, not the grouped
    /// `Operation::name` labels.
    op_names: Vec<Vec<u8>>,
    /// `q`/`Q` graphics-state stack depth.
    gsave_depth: usize,
    /// Inside `BT`/`ET`.
    in_text: bool,
    /// Inside `BX`/`EX`.
    in_compat: bool,
    /// `BMC`/`BDC`/`EMC` nesting depth (tracked for balance only).
    mc_depth: usize,
}

impl<'a> ContentParser<'a> {
    fn new(limits: &'a Limits) -> Self {
        ContentParser {
            limits,
            operands: Vec::new(),
            ops: Vec::new(),
            tokens_seen: 0,
            op_names: Vec::new(),
            gsave_depth: 0,
            in_text: false,
            in_compat: false,
            mc_depth: 0,
        }
    }

    fn parse_stream(&mut self, data: &[u8]) -> Result<(), Error> {
        let mut lexer = Lexer::new(data, self.limits);
        loop {
            match lexer.next_token()? {
                None => break,
                Some(tok) => {
                    self.tokens_seen += 1;
                    self.handle_token(&mut lexer, tok)?;
                }
            }
            if self.ops.len() > self.limits.max_operations {
                return Err(Error::LimitExceeded {
                    limit: "max_operations",
                    max: self.limits.max_operations as u64,
                });
            }
            if self.operands.len() > self.limits.max_operand_stack {
                return Err(Error::LimitExceeded {
                    limit: "max_operand_stack",
                    max: self.limits.max_operand_stack as u64,
                });
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<Vec<Operation>, Error> {
        if !self.operands.is_empty() {
            return Err(Error::Parse(format!(
                "content stream ends with {} un-consumed operand(s)",
                self.operands.len()
            )));
        }
        if self.gsave_depth != 0 {
            return Err(Error::Structure(format!(
                "content stream ends with {} unmatched 'q' operator(s)",
                self.gsave_depth
            )));
        }
        if self.in_text {
            return Err(Error::Structure(
                "content stream ends inside a BT/ET text block".into(),
            ));
        }
        if self.mc_depth != 0 {
            return Err(Error::Structure(format!(
                "content stream ends with {} unmatched marked-content block(s)",
                self.mc_depth
            )));
        }
        if self.in_compat {
            return Err(Error::Structure(
                "content stream ends inside a BX/EX compatibility section".into(),
            ));
        }
        Ok(self.ops)
    }

    fn handle_token(&mut self, lexer: &mut Lexer, tok: Token) -> Result<(), Error> {
        let offset = tok.offset();
        match tok {
            Token::Word { bytes, .. } => {
                if let Some(i) = parse_int(&bytes) {
                    self.operands.push(Object::Integer(i));
                } else if let Some(f) = parse_real(&bytes) {
                    self.operands.push(Object::Real(f));
                } else if bytes == b"true" {
                    self.operands.push(Object::Bool(true));
                } else if bytes == b"false" {
                    self.operands.push(Object::Bool(false));
                } else if bytes == b"null" {
                    self.operands.push(Object::Null);
                } else {
                    // Not a primitive value → an operator token.
                    self.dispatch_operator(&bytes, offset)?;
                }
            }
            Token::Name { value, .. } => self.operands.push(Object::Name(value)),
            Token::LiteralString { value, .. } | Token::HexString { value, .. } => {
                self.operands.push(Object::String(value))
            }
            Token::ArrayOpen { .. } => {
                let arr = self.parse_array(lexer, 1)?;
                self.operands.push(arr);
            }
            Token::DictOpen { .. } => {
                let dict = self.parse_dict(lexer, 1)?;
                self.operands.push(dict);
            }
            Token::ArrayClose { .. } | Token::DictClose { .. } => {
                return Err(Error::Parse(format!(
                    "unbalanced closing delimiter in content stream at byte {offset}"
                )));
            }
        }
        Ok(())
    }

    /// Parse a nested `[ … ]` array operand (used by `TJ`, `d`, …).
    /// Only direct objects may appear; `n g R` refs and streams cannot
    /// occur inside a content stream.
    fn parse_array(&mut self, lexer: &mut Lexer, depth: u32) -> Result<Object, Error> {
        if depth > self.limits.max_nesting_depth {
            return Err(Error::LimitExceeded {
                limit: "max_nesting_depth (content array)",
                max: self.limits.max_nesting_depth as u64,
            });
        }
        let mut items = Vec::new();
        loop {
            if items.len() >= self.limits.max_array_elements {
                return Err(Error::LimitExceeded {
                    limit: "max_array_elements (content)",
                    max: self.limits.max_array_elements as u64,
                });
            }
            match lexer.next_token()? {
                None => {
                    return Err(Error::UnexpectedEof {
                        offset: lexer.pos(),
                        context: "content array",
                    })
                }
                Some(Token::ArrayClose { .. }) => return Ok(Object::Array(items)),
                Some(Token::Word { bytes, offset }) => {
                    items.push(word_as_operand(&bytes, offset)?);
                }
                Some(Token::Name { value, .. }) => items.push(Object::Name(value)),
                Some(Token::LiteralString { value, .. }) | Some(Token::HexString { value, .. }) => {
                    items.push(Object::String(value))
                }
                Some(Token::ArrayOpen { .. }) => items.push(self.parse_array(lexer, depth + 1)?),
                Some(Token::DictOpen { .. }) => items.push(self.parse_dict(lexer, depth + 1)?),
                Some(Token::DictClose { offset }) => {
                    return Err(Error::Parse(format!(
                        "unbalanced '>>' in array at byte {offset}"
                    )))
                }
            }
        }
    }

    /// Parse a nested `<< … >>` dict operand (e.g. `BDC` properties).
    /// A `null` value means the key is absent (§7.3.7).
    fn parse_dict(&mut self, lexer: &mut Lexer, depth: u32) -> Result<Object, Error> {
        if depth > self.limits.max_nesting_depth {
            return Err(Error::LimitExceeded {
                limit: "max_nesting_depth (content dict)",
                max: self.limits.max_nesting_depth as u64,
            });
        }
        let mut dict = Dict::new();
        loop {
            if dict.len() >= self.limits.max_dict_entries {
                return Err(Error::LimitExceeded {
                    limit: "max_dict_entries (content)",
                    max: self.limits.max_dict_entries as u64,
                });
            }
            match lexer.next_token()? {
                None => {
                    return Err(Error::UnexpectedEof {
                        offset: lexer.pos(),
                        context: "content dict",
                    })
                }
                Some(Token::DictClose { .. }) => return Ok(Object::Dict(dict)),
                Some(Token::Name { value: key, offset }) => {
                    let value = self.parse_direct_object(lexer, depth + 1, offset)?;
                    if value != Object::Null {
                        dict.insert(key, value);
                    }
                }
                Some(other) => {
                    return Err(Error::Parse(format!(
                        "expected name key in dict, found token at byte {}",
                        other.offset()
                    )))
                }
            }
        }
    }

    /// Parse one direct object in a dict/array value position.
    fn parse_direct_object(
        &mut self,
        lexer: &mut Lexer,
        depth: u32,
        offset: usize,
    ) -> Result<Object, Error> {
        match lexer.next_token()? {
            None => Err(Error::UnexpectedEof {
                offset,
                context: "dict value",
            }),
            Some(Token::Word { bytes, offset }) => word_as_operand(&bytes, offset),
            Some(Token::Name { value, .. }) => Ok(Object::Name(value)),
            Some(Token::LiteralString { value, .. }) | Some(Token::HexString { value, .. }) => {
                Ok(Object::String(value))
            }
            Some(Token::ArrayOpen { .. }) => self.parse_array(lexer, depth + 1),
            Some(Token::DictOpen { .. }) => self.parse_dict(lexer, depth + 1),
            Some(Token::ArrayClose { .. }) | Some(Token::DictClose { .. }) => Err(Error::Parse(
                format!("unbalanced closing delimiter in dict at byte {offset}"),
            )),
        }
    }

    // ---------------------------------------------------------------
    // Operator dispatch
    // ---------------------------------------------------------------

    fn dispatch_operator(&mut self, op: &[u8], offset: usize) -> Result<(), Error> {
        let n_operands = self.operands.len();
        let result = match op {
            // Graphics state & matrix
            b"q" => {
                self.expect_operands(0, n_operands, op, offset)?;
                self.gsave_depth += 1;
                if self.gsave_depth > self.limits.max_gsave_depth {
                    return Err(Error::LimitExceeded {
                        limit: "max_gsave_depth",
                        max: self.limits.max_gsave_depth as u64,
                    });
                }
                Operation::SaveState
            }
            b"Q" => {
                self.expect_operands(0, n_operands, op, offset)?;
                if self.gsave_depth == 0 {
                    return Err(Error::Structure("'Q' without matching 'q'".into()));
                }
                self.gsave_depth -= 1;
                Operation::RestoreState
            }
            b"cm" => {
                self.expect_operands(6, n_operands, op, offset)?;
                let f = self.take_number()?;
                let e = self.take_number()?;
                let d = self.take_number()?;
                let c = self.take_number()?;
                let b = self.take_number()?;
                let a = self.take_number()?;
                Operation::ConcatMatrix(Matrix::new(a, b, c, d, e, f))
            }
            b"w" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::SetLineWidth(self.take_number()?)
            }
            b"J" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::SetLineCap(self.take_int()?)
            }
            b"j" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::SetLineJoin(self.take_int()?)
            }
            b"M" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::SetMiterLimit(self.take_number()?)
            }
            b"d" => {
                self.expect_operands(2, n_operands, op, offset)?;
                let phase = self.take_number()?;
                let arr = self.operands.pop().unwrap();
                let dashes = self.array_to_numbers(arr, "d dash array")?;
                Operation::SetDash { dashes, phase }
            }
            b"ri" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::SetRenderingIntent(self.take_name()?)
            }
            b"i" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::SetFlatness(self.take_number()?)
            }
            b"gs" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::SetExtGState(self.take_name()?)
            }

            // Path
            b"m" => {
                self.expect_operands(2, n_operands, op, offset)?;
                let y = self.take_number()?;
                let x = self.take_number()?;
                Operation::MoveTo(x, y)
            }
            b"l" => {
                self.expect_operands(2, n_operands, op, offset)?;
                let y = self.take_number()?;
                let x = self.take_number()?;
                Operation::LineTo(x, y)
            }
            b"c" => {
                self.expect_operands(6, n_operands, op, offset)?;
                let y3 = self.take_number()?;
                let x3 = self.take_number()?;
                let y2 = self.take_number()?;
                let x2 = self.take_number()?;
                let y1 = self.take_number()?;
                let x1 = self.take_number()?;
                Operation::CurveTo(x1, y1, x2, y2, x3, y3)
            }
            b"v" => {
                self.expect_operands(4, n_operands, op, offset)?;
                let y3 = self.take_number()?;
                let x3 = self.take_number()?;
                let y2 = self.take_number()?;
                let x2 = self.take_number()?;
                Operation::CurveToV(x2, y2, x3, y3)
            }
            b"y" => {
                self.expect_operands(4, n_operands, op, offset)?;
                let y3 = self.take_number()?;
                let x3 = self.take_number()?;
                let y1 = self.take_number()?;
                let x1 = self.take_number()?;
                Operation::CurveToY(x1, y1, x3, y3)
            }
            b"h" => {
                self.expect_operands(0, n_operands, op, offset)?;
                Operation::ClosePath
            }
            b"re" => {
                self.expect_operands(4, n_operands, op, offset)?;
                let h = self.take_number()?;
                let w = self.take_number()?;
                let y = self.take_number()?;
                let x = self.take_number()?;
                Operation::Rectangle(x, y, w, h)
            }
            b"S" => self.zero_operand(n_operands, op, offset, Operation::Stroke)?,
            b"s" => self.zero_operand(n_operands, op, offset, Operation::CloseAndStroke)?,
            b"f" | b"F" => self.zero_operand(n_operands, op, offset, Operation::Fill)?,
            b"f*" => self.zero_operand(n_operands, op, offset, Operation::FillEvenOdd)?,
            b"B" => self.zero_operand(n_operands, op, offset, Operation::FillAndStroke)?,
            b"B*" => self.zero_operand(n_operands, op, offset, Operation::FillAndStrokeEvenOdd)?,
            b"b" => self.zero_operand(n_operands, op, offset, Operation::CloseFillAndStroke)?,
            b"b*" => {
                self.zero_operand(n_operands, op, offset, Operation::CloseFillAndStrokeEvenOdd)?
            }
            b"n" => self.zero_operand(n_operands, op, offset, Operation::EndPath)?,
            b"W" => self.zero_operand(n_operands, op, offset, Operation::Clip)?,
            b"W*" => self.zero_operand(n_operands, op, offset, Operation::ClipEvenOdd)?,

            // Colour
            b"G" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::SetGrayStroke(self.take_number()?)
            }
            b"g" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::SetGrayFill(self.take_number()?)
            }
            b"RG" => {
                self.expect_operands(3, n_operands, op, offset)?;
                let b_ = self.take_number()?;
                let g_ = self.take_number()?;
                let r = self.take_number()?;
                Operation::SetRgbStroke(r, g_, b_)
            }
            b"rg" => {
                self.expect_operands(3, n_operands, op, offset)?;
                let b_ = self.take_number()?;
                let g_ = self.take_number()?;
                let r = self.take_number()?;
                Operation::SetRgbFill(r, g_, b_)
            }
            b"K" => {
                self.expect_operands(4, n_operands, op, offset)?;
                let k_ = self.take_number()?;
                let y_ = self.take_number()?;
                let m_ = self.take_number()?;
                let c_ = self.take_number()?;
                Operation::SetCmykStroke(c_, m_, y_, k_)
            }
            b"k" => {
                self.expect_operands(4, n_operands, op, offset)?;
                let k_ = self.take_number()?;
                let y_ = self.take_number()?;
                let m_ = self.take_number()?;
                let c_ = self.take_number()?;
                Operation::SetCmykFill(c_, m_, y_, k_)
            }
            b"CS" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::SetColorSpaceStroke(self.take_name()?)
            }
            b"cs" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::SetColorSpaceFill(self.take_name()?)
            }
            b"SC" | b"SCN" => {
                // Operand count depends on the current colour space;
                // SCN may take a trailing pattern name. The parser
                // keeps the operands; resolving the colour space is a
                // later stage.
                let ops = std::mem::take(&mut self.operands);
                self.validate_color_operands(&ops, op, offset, matches!(op, b"SCN"))?;
                Operation::SetColorStroke(ops)
            }
            b"sc" | b"scn" => {
                let ops = std::mem::take(&mut self.operands);
                self.validate_color_operands(&ops, op, offset, matches!(op, b"scn"))?;
                Operation::SetColorFill(ops)
            }

            // Text
            b"BT" => {
                self.expect_operands(0, n_operands, op, offset)?;
                if self.in_text {
                    return Err(Error::Structure("nested BT without ET".into()));
                }
                self.in_text = true;
                Operation::BeginText
            }
            b"ET" => {
                self.expect_operands(0, n_operands, op, offset)?;
                if !self.in_text {
                    return Err(Error::Structure("ET without BT".into()));
                }
                self.in_text = false;
                Operation::EndText
            }
            b"Tc" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::SetCharSpacing(self.take_number()?)
            }
            b"Tw" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::SetWordSpacing(self.take_number()?)
            }
            b"Tz" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::SetHorizontalScaling(self.take_number()?)
            }
            b"TL" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::SetTextLeading(self.take_number()?)
            }
            b"Tf" => {
                self.expect_operands(2, n_operands, op, offset)?;
                let size = self.take_number()?;
                let font = self.take_name()?;
                Operation::SetTextFont { font, size }
            }
            b"Tr" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::SetTextRenderingMode(self.take_int()?)
            }
            b"Ts" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::SetTextRise(self.take_number()?)
            }
            b"Td" => {
                self.expect_operands(2, n_operands, op, offset)?;
                let ty = self.take_number()?;
                let tx = self.take_number()?;
                Operation::MoveTextPosition(tx, ty)
            }
            b"TD" => {
                self.expect_operands(2, n_operands, op, offset)?;
                let ty = self.take_number()?;
                let tx = self.take_number()?;
                Operation::MoveTextPositionAndSetLeading(tx, ty)
            }
            b"Tm" => {
                self.expect_operands(6, n_operands, op, offset)?;
                let f = self.take_number()?;
                let e = self.take_number()?;
                let d = self.take_number()?;
                let c = self.take_number()?;
                let b = self.take_number()?;
                let a = self.take_number()?;
                Operation::SetTextMatrix(Matrix::new(a, b, c, d, e, f))
            }
            b"T*" => {
                self.expect_operands(0, n_operands, op, offset)?;
                Operation::NextLine
            }
            b"Tj" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::ShowText(self.take_string()?)
            }
            b"TJ" => {
                self.expect_operands(1, n_operands, op, offset)?;
                let arr = self.operands.pop().unwrap();
                Operation::ShowTextAdjusted(self.tj_array(arr)?)
            }
            b"'" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::NextLineShowText(self.take_string()?)
            }
            b"\"" => {
                self.expect_operands(3, n_operands, op, offset)?;
                let text = self.take_string()?;
                let char_spacing = self.take_number()?;
                let word_spacing = self.take_number()?;
                Operation::SetSpacingNextLineShowText {
                    word_spacing,
                    char_spacing,
                    text,
                }
            }

            // Type 3 glyph-program ops (§9.6.4) — only meaningful
            // inside a /CharProcs stream; kept as data here.
            b"d0" => {
                self.expect_operands(2, n_operands, op, offset)?;
                let wy = self.take_number()?;
                let wx = self.take_number()?;
                Operation::Type3SetWidth { wx, wy }
            }
            b"d1" => {
                self.expect_operands(6, n_operands, op, offset)?;
                let ury = self.take_number()?;
                let urx = self.take_number()?;
                let lly = self.take_number()?;
                let llx = self.take_number()?;
                let wy = self.take_number()?;
                let wx = self.take_number()?;
                Operation::Type3SetWidthAndBox {
                    wx,
                    wy,
                    llx,
                    lly,
                    urx,
                    ury,
                }
            }

            // External objects & marked content
            b"Do" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::Do(self.take_name()?)
            }
            b"sh" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::Shading(self.take_name()?)
            }
            b"MP" => {
                self.expect_operands(1, n_operands, op, offset)?;
                Operation::MarkedContentPoint(self.take_name()?)
            }
            b"DP" => {
                self.expect_operands(2, n_operands, op, offset)?;
                let properties = self.take_dict_or_name("DP properties")?;
                let tag = self.take_name()?;
                Operation::DesignatedMarkedContentPoint { tag, properties }
            }
            b"BMC" => {
                self.expect_operands(1, n_operands, op, offset)?;
                self.mc_depth += 1;
                Operation::BeginMarkedContent(self.take_name()?)
            }
            b"BDC" => {
                self.expect_operands(2, n_operands, op, offset)?;
                let properties = self.take_dict_or_name("BDC properties")?;
                let tag = self.take_name()?;
                self.mc_depth += 1;
                Operation::BeginMarkedContentWithProps { tag, properties }
            }
            b"EMC" => {
                self.expect_operands(0, n_operands, op, offset)?;
                if self.mc_depth == 0 {
                    return Err(Error::Structure("EMC without BMC/BDC".into()));
                }
                self.mc_depth -= 1;
                Operation::EndMarkedContent
            }
            b"BX" => {
                self.expect_operands(0, n_operands, op, offset)?;
                if self.in_compat {
                    return Err(Error::Structure("nested BX without EX".into()));
                }
                self.in_compat = true;
                Operation::BeginCompatibility
            }
            b"EX" => {
                self.expect_operands(0, n_operands, op, offset)?;
                if !self.in_compat {
                    return Err(Error::Structure("EX without BX".into()));
                }
                self.in_compat = false;
                Operation::EndCompatibility
            }

            // Inline images (§8.9.7): the data between ID and EI is
            // binary and cannot be tokenized reliably without the
            // image dictionary's keys, so the construct is refused
            // outright rather than guessed.
            b"BI" | b"ID" | b"EI" => {
                return Err(Error::Unsupported {
                    feature: "inline image (BI/ID/EI)",
                    detail: format!(
                        "operator '{}' at byte {offset}",
                        String::from_utf8_lossy(op)
                    ),
                })
            }

            _ => {
                if self.in_compat {
                    let ops = std::mem::take(&mut self.operands);
                    Operation::Unknown {
                        operator: op.to_vec(),
                        operands: ops,
                    }
                } else {
                    return Err(Error::Parse(format!(
                        "unknown operator '{}' at byte {offset}",
                        String::from_utf8_lossy(op)
                    )));
                }
            }
        };
        self.op_names.push(op.to_vec());
        self.ops.push(result);
        Ok(())
    }

    fn zero_operand(
        &mut self,
        n_operands: usize,
        op: &[u8],
        offset: usize,
        out: Operation,
    ) -> Result<Operation, Error> {
        self.expect_operands(0, n_operands, op, offset)?;
        Ok(out)
    }

    /// `SC`/`SCN`/`sc`/`scn` operand validation.
    ///
    /// - `SC`/`sc`: only numeric components (no trailing name).
    /// - `SCN`/`scn`: numeric components plus an *optional* trailing
    ///   pattern name (the pattern name is what the `N` in the operator
    ///   name refers to — §8.6.8 Table 74–75).
    ///
    /// The component count itself is *not* bounded at this stage:
    /// DeviceN colour spaces may take more than 4 components, and the
    /// real arity depends on the resolved colour space. `DeviceN`
    /// rendering is currently `Error::Unsupported`, but parsing is not
    /// — the operand list is kept intact for a later stage to validate.
    /// The only bound is the generic `max_operand_stack` limit.
    fn validate_color_operands(
        &self,
        ops: &[Object],
        op: &[u8],
        offset: usize,
        allow_name: bool,
    ) -> Result<(), Error> {
        let (comps, name) = match ops.split_last() {
            Some((last @ Object::Name(_), rest)) if allow_name => (rest, Some(last)),
            _ => (ops, None),
        };
        for c in comps {
            if !matches!(c, Object::Integer(_) | Object::Real(_)) {
                return Err(Error::Parse(format!(
                    "operator '{}' operands must be numbers{} at byte {offset}",
                    String::from_utf8_lossy(op),
                    if allow_name {
                        " plus an optional pattern name"
                    } else {
                        ""
                    }
                )));
            }
        }
        let _ = name;
        Ok(())
    }

    /// `DP`/`BDC` properties operand: a dictionary or a name (a
    /// `/Properties` resource lookup). Other types are rejected.
    fn take_dict_or_name(&mut self, context: &str) -> Result<Object, Error> {
        match self.operands.pop() {
            Some(obj @ (Object::Dict(_) | Object::Name(_))) => Ok(obj),
            _ => Err(Error::Parse(format!(
                "{context} operand must be a dictionary or name"
            ))),
        }
    }

    fn expect_operands(
        &mut self,
        expected: usize,
        actual: usize,
        op: &[u8],
        offset: usize,
    ) -> Result<(), Error> {
        if actual != expected {
            return Err(Error::Parse(format!(
                "operator '{}' expects {} operand(s), found {} at byte {offset}",
                String::from_utf8_lossy(op),
                expected,
                actual
            )));
        }
        Ok(())
    }

    // ---------------------------------------------------------------
    // Operand extraction (pop order is last-operand-first)
    // ---------------------------------------------------------------

    fn take_number(&mut self) -> Result<f64, Error> {
        match self.operands.pop() {
            Some(Object::Integer(i)) => Ok(i as f64),
            Some(Object::Real(r)) => Ok(r),
            _ => Err(Error::Parse("expected numeric operand".into())),
        }
    }

    fn take_int(&mut self) -> Result<i64, Error> {
        match self.operands.pop() {
            Some(Object::Integer(i)) => Ok(i),
            _ => Err(Error::Parse("expected integer operand".into())),
        }
    }

    fn take_name(&mut self) -> Result<Name, Error> {
        match self.operands.pop() {
            Some(Object::Name(n)) => Ok(n),
            _ => Err(Error::Parse("expected name operand".into())),
        }
    }

    fn take_string(&mut self) -> Result<Vec<u8>, Error> {
        match self.operands.pop() {
            Some(Object::String(s)) => Ok(s),
            _ => Err(Error::Parse("expected string operand".into())),
        }
    }

    fn array_to_numbers(&self, obj: Object, context: &str) -> Result<Vec<f64>, Error> {
        match obj {
            Object::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for it in items {
                    match it {
                        Object::Integer(i) => out.push(i as f64),
                        Object::Real(r) => out.push(r),
                        _ => return Err(Error::Parse(format!("expected number in {context}"))),
                    }
                }
                Ok(out)
            }
            _ => Err(Error::Parse(format!("expected array for {context}"))),
        }
    }

    fn tj_array(&self, obj: Object) -> Result<Vec<TJItem>, Error> {
        match obj {
            Object::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for it in items {
                    match it {
                        Object::String(s) => out.push(TJItem::Text(s)),
                        Object::Integer(i) => out.push(TJItem::Kern(i as f64)),
                        Object::Real(r) => out.push(TJItem::Kern(r)),
                        _ => {
                            return Err(Error::Parse(
                                "TJ array elements must be strings or numbers".into(),
                            ))
                        }
                    }
                }
                Ok(out)
            }
            _ => Err(Error::Parse("TJ expects an array".into())),
        }
    }
}

/// Interpret a word token inside a content operand position: number,
/// boolean, null — or an error (operators cannot appear where an
/// operand is required).
fn word_as_operand(bytes: &[u8], offset: usize) -> Result<Object, Error> {
    if let Some(i) = parse_int(bytes) {
        return Ok(Object::Integer(i));
    }
    if let Some(f) = parse_real(bytes) {
        return Ok(Object::Real(f));
    }
    match bytes {
        b"true" => Ok(Object::Bool(true)),
        b"false" => Ok(Object::Bool(false)),
        b"null" => Ok(Object::Null),
        _ => Err(Error::Parse(format!(
            "keyword '{}' cannot be an operand at byte {offset}",
            String::from_utf8_lossy(bytes)
        ))),
    }
}

fn parse_int(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() {
        return None;
    }
    let rest = match bytes[0] {
        b'+' | b'-' => &bytes[1..],
        _ => bytes,
    };
    if rest.is_empty() || !rest.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse::<i64>().ok()
}

fn parse_real(bytes: &[u8]) -> Option<f64> {
    if bytes.is_empty() {
        return None;
    }
    if !bytes
        .iter()
        .all(|b| b.is_ascii_digit() || *b == b'.' || *b == b'+' || *b == b'-')
    {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse::<f64>().ok()
}

// --------------------------------------------------------------------
// Deterministic display for the `operations` CLI command
// --------------------------------------------------------------------

fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn fmt_name(n: &[u8]) -> String {
    format!("/{}", escape_name(n))
}

/// Names are byte strings; render `#xx` escapes for bytes outside the
/// printable, non-delimiter subset (inverse of lexer escapes).
fn escape_name(n: &[u8]) -> String {
    let mut out = String::new();
    for &b in n {
        if matches!(b, 0x21..=0x7E)
            && !matches!(
                b,
                b'#' | b'%' | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/'
            )
        {
            out.push(b as char);
        } else {
            out.push_str(&format!("#{b:02X}"));
        }
    }
    out
}

/// Maximum string bytes shown by `escape_string`; longer strings are
/// truncated with a `...` marker so CLI output stays bounded.
const MAX_DISPLAY_STRING: usize = 64;

/// Literal string shown with escapes; non-printable bytes become
/// `\xNN`. Control characters are never passed through to the terminal.
fn escape_string(s: &[u8]) -> String {
    let mut out = String::from("(\"");
    let shown = s.len().min(MAX_DISPLAY_STRING);
    for &b in &s[..shown] {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            0x20..=0x7E => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02X}")),
        }
    }
    if s.len() > shown {
        out.push_str("...");
    }
    out.push_str("\")");
    out
}

fn fmt_operand(o: &Object) -> String {
    match o {
        Object::Null => "null".into(),
        Object::Bool(b) => b.to_string(),
        Object::Integer(i) => i.to_string(),
        Object::Real(r) => fmt_num(*r),
        Object::Name(n) => fmt_name(n),
        Object::String(s) => escape_string(s),
        Object::Array(items) => {
            let inner: Vec<String> = items.iter().map(fmt_operand).collect();
            format!("[{}]", inner.join(" "))
        }
        Object::Dict(d) => {
            let inner: Vec<String> = d
                .iter()
                .map(|(k, v)| format!("{} {}", fmt_name(k), fmt_operand(v)))
                .collect();
            format!("<< {} >>", inner.join(" "))
        }
        Object::Stream { .. } => "<stream>".into(),
        Object::Ref(r) => format!("{r}"),
    }
}

impl fmt::Display for TJItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TJItem::Text(s) => write!(f, "{}", escape_string(s)),
            TJItem::Kern(k) => write!(f, "{}", fmt_num(*k)),
        }
    }
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operation::SaveState => write!(f, "q"),
            Operation::RestoreState => write!(f, "Q"),
            Operation::ConcatMatrix(m) => write!(f, "{m} cm"),
            Operation::SetLineWidth(w) => write!(f, "{} w", fmt_num(*w)),
            Operation::SetLineCap(c) => write!(f, "{c} J"),
            Operation::SetLineJoin(j) => write!(f, "{j} j"),
            Operation::SetMiterLimit(m) => write!(f, "{} M", fmt_num(*m)),
            Operation::SetDash { dashes, phase } => {
                let ds: Vec<String> = dashes.iter().map(|d| fmt_num(*d)).collect();
                write!(f, "[{}] {} d", ds.join(" "), fmt_num(*phase))
            }
            Operation::SetRenderingIntent(n) => write!(f, "{} ri", fmt_name(n)),
            Operation::SetFlatness(i) => write!(f, "{} i", fmt_num(*i)),
            Operation::SetExtGState(n) => write!(f, "{} gs", fmt_name(n)),
            Operation::MoveTo(x, y) => write!(f, "{} {} m", fmt_num(*x), fmt_num(*y)),
            Operation::LineTo(x, y) => write!(f, "{} {} l", fmt_num(*x), fmt_num(*y)),
            Operation::CurveTo(x1, y1, x2, y2, x3, y3) => write!(
                f,
                "{} {} {} {} {} {} c",
                fmt_num(*x1),
                fmt_num(*y1),
                fmt_num(*x2),
                fmt_num(*y2),
                fmt_num(*x3),
                fmt_num(*y3)
            ),
            Operation::CurveToV(x2, y2, x3, y3) => write!(
                f,
                "{} {} {} {} v",
                fmt_num(*x2),
                fmt_num(*y2),
                fmt_num(*x3),
                fmt_num(*y3)
            ),
            Operation::CurveToY(x1, y1, x3, y3) => write!(
                f,
                "{} {} {} {} y",
                fmt_num(*x1),
                fmt_num(*y1),
                fmt_num(*x3),
                fmt_num(*y3)
            ),
            Operation::ClosePath => write!(f, "h"),
            Operation::Rectangle(x, y, w, h) => write!(
                f,
                "{} {} {} {} re",
                fmt_num(*x),
                fmt_num(*y),
                fmt_num(*w),
                fmt_num(*h)
            ),
            Operation::Stroke => write!(f, "S"),
            Operation::CloseAndStroke => write!(f, "s"),
            Operation::Fill => write!(f, "f"),
            Operation::FillEvenOdd => write!(f, "f*"),
            Operation::FillAndStroke => write!(f, "B"),
            Operation::FillAndStrokeEvenOdd => write!(f, "B*"),
            Operation::CloseFillAndStroke => write!(f, "b"),
            Operation::CloseFillAndStrokeEvenOdd => write!(f, "b*"),
            Operation::EndPath => write!(f, "n"),
            Operation::Clip => write!(f, "W"),
            Operation::ClipEvenOdd => write!(f, "W*"),
            Operation::SetGrayStroke(g) => write!(f, "{} G", fmt_num(*g)),
            Operation::SetGrayFill(g) => write!(f, "{} g", fmt_num(*g)),
            Operation::SetRgbStroke(r, g, b) => {
                write!(f, "{} {} {} RG", fmt_num(*r), fmt_num(*g), fmt_num(*b))
            }
            Operation::SetRgbFill(r, g, b) => {
                write!(f, "{} {} {} rg", fmt_num(*r), fmt_num(*g), fmt_num(*b))
            }
            Operation::SetCmykStroke(c, m, y, k) => write!(
                f,
                "{} {} {} {} K",
                fmt_num(*c),
                fmt_num(*m),
                fmt_num(*y),
                fmt_num(*k)
            ),
            Operation::SetCmykFill(c, m, y, k) => write!(
                f,
                "{} {} {} {} k",
                fmt_num(*c),
                fmt_num(*m),
                fmt_num(*y),
                fmt_num(*k)
            ),
            Operation::SetColorSpaceStroke(n) => write!(f, "{} CS", fmt_name(n)),
            Operation::SetColorSpaceFill(n) => write!(f, "{} cs", fmt_name(n)),
            Operation::SetColorStroke(ops) => {
                let os: Vec<String> = ops.iter().map(fmt_operand).collect();
                write!(f, "{} SC", os.join(" "))
            }
            Operation::SetColorFill(ops) => {
                let os: Vec<String> = ops.iter().map(fmt_operand).collect();
                write!(f, "{} sc", os.join(" "))
            }
            Operation::BeginText => write!(f, "BT"),
            Operation::EndText => write!(f, "ET"),
            Operation::SetCharSpacing(v) => write!(f, "{} Tc", fmt_num(*v)),
            Operation::SetWordSpacing(v) => write!(f, "{} Tw", fmt_num(*v)),
            Operation::SetHorizontalScaling(v) => write!(f, "{} Tz", fmt_num(*v)),
            Operation::SetTextLeading(v) => write!(f, "{} TL", fmt_num(*v)),
            Operation::SetTextFont { font, size } => {
                write!(f, "{} {} Tf", fmt_name(font), fmt_num(*size))
            }
            Operation::SetTextRenderingMode(m) => write!(f, "{m} Tr"),
            Operation::SetTextRise(v) => write!(f, "{} Ts", fmt_num(*v)),
            Operation::MoveTextPosition(tx, ty) => {
                write!(f, "{} {} Td", fmt_num(*tx), fmt_num(*ty))
            }
            Operation::MoveTextPositionAndSetLeading(tx, ty) => {
                write!(f, "{} {} TD", fmt_num(*tx), fmt_num(*ty))
            }
            Operation::SetTextMatrix(m) => write!(f, "{m} Tm"),
            Operation::NextLine => write!(f, "T*"),
            Operation::ShowText(s) => write!(f, "{} Tj", escape_string(s)),
            Operation::ShowTextAdjusted(items) => {
                let is: Vec<String> = items.iter().map(ToString::to_string).collect();
                write!(f, "[{}] TJ", is.join(" "))
            }
            Operation::NextLineShowText(s) => write!(f, "{} '", escape_string(s)),
            Operation::SetSpacingNextLineShowText {
                word_spacing,
                char_spacing,
                text,
            } => write!(
                f,
                "{} {} {} \"",
                fmt_num(*word_spacing),
                fmt_num(*char_spacing),
                escape_string(text)
            ),
            Operation::Type3SetWidth { wx, wy } => {
                write!(f, "{} {} d0", fmt_num(*wx), fmt_num(*wy))
            }
            Operation::Type3SetWidthAndBox {
                wx,
                wy,
                llx,
                lly,
                urx,
                ury,
            } => write!(
                f,
                "{} {} {} {} {} {} d1",
                fmt_num(*wx),
                fmt_num(*wy),
                fmt_num(*llx),
                fmt_num(*lly),
                fmt_num(*urx),
                fmt_num(*ury)
            ),
            Operation::Do(n) => write!(f, "{} Do", fmt_name(n)),
            Operation::Shading(n) => write!(f, "{} sh", fmt_name(n)),
            Operation::MarkedContentPoint(n) => write!(f, "{} MP", fmt_name(n)),
            Operation::DesignatedMarkedContentPoint { tag, properties } => {
                write!(f, "{} {} DP", fmt_name(tag), fmt_operand(properties))
            }
            Operation::BeginMarkedContent(n) => write!(f, "{} BMC", fmt_name(n)),
            Operation::BeginMarkedContentWithProps { tag, properties } => {
                write!(f, "{} {} BDC", fmt_name(tag), fmt_operand(properties))
            }
            Operation::EndMarkedContent => write!(f, "EMC"),
            Operation::BeginCompatibility => write!(f, "BX"),
            Operation::EndCompatibility => write!(f, "EX"),
            Operation::Unknown { operator, operands } => {
                let os: Vec<String> = operands.iter().map(fmt_operand).collect();
                let joined = if os.is_empty() {
                    String::new()
                } else {
                    format!("{} ", os.join(" "))
                };
                write!(f, "{joined}{}", String::from_utf8_lossy(operator))
            }
        }
    }
}
