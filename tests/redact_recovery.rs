//! Redaction of documents opened in recovery mode (R0 in
//! docs/compatibility-roadmap.md; the policy is in
//! docs/redaction-safety-model.md § Recovery mode).
//!
//! Drawing a malformed document and redacting it safely are different
//! claims. Every case below is a small synthetic document in two
//! variants that differ only in a secret drawn inside the rect (equal
//! lengths, so nothing else moves). Each is opened strictly and with
//! `Document::from_bytes_recovering`, rendered, and redacted; the
//! expected outcome of every step is stated per case. The oracle for
//! a redaction:
//!
//! - refused: both variants refused with the same message, the
//!   message names the expected cause, and it carries nothing of the
//!   secret. `redact` returns no bytes on an error, so no partial
//!   output exists.
//! - redacted: the two outputs are byte-identical (nothing of the
//!   secret reaches the output by any route), the output parses
//!   strictly, renders, applies no recovery rule, still draws the
//!   public text outside the rect, and holds no copy of the secret.
//!
//! A case is redacted in recovery mode only where the recovered
//! reading is the one the major readers share and the redaction walker
//! sees every drawing operation: an ignored `Q` without a saved state,
//! a rebuilt cross-reference table (Poppler, MuPDF and qpdf pick the
//! same definitions). Where recovery infers or drops something — a
//! default page box, a dropped operator, a substituted font, a skipped
//! annotation, a plain marked-content sequence, an ignored soft mask —
//! the redaction refuses. Strict mode is unchanged by any of it: every
//! strict expectation is the behaviour of the strict paths today.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{
    extract_text, redact, render_page, Document, Error, Limits, OpenOptions, RecoveryRule,
    RedactOptions, RenderOptions, TextOptions, Warning,
};

const RECT: (f64, f64, f64, f64) = (60.0, 60.0, 140.0, 140.0);
/// Text kept outside the rect, so every output has something to keep.
const KEPT: &str = "BT /F1 10 Tf 10 20 Td (PUBLICWORD) Tj ET";

/// The secret of each variant: different text, equal length.
fn secret(v: u32) -> &'static str {
    ["", "ALPHA", "BRAVO"][v as usize]
}

/// The secret drawn inside the rect.
fn shown(v: u32) -> String {
    format!("BT /F1 10 Tf 80 100 Td ({}) Tj ET", secret(v))
}

fn font_objs() -> Vec<(u32, Vec<u8>)> {
    let ff = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/liberation/LiberationSans-Regular.ttf"
    ))
    .unwrap();
    let widths = vec!["600"; 95].join(" ");
    vec![
        (
            20,
            format!(
                "<< /Type /Font /Subtype /TrueType /BaseFont /LiberationSans /FirstChar 32 \
                 /LastChar 126 /Widths [{widths}] /Encoding /WinAnsiEncoding \
                 /FontDescriptor 21 0 R >>"
            )
            .into_bytes(),
        ),
        (
            21,
            b"<< /Type /FontDescriptor /FontName /LiberationSans /Flags 32 \
              /FontBBox [-200 -300 1200 1000] /ItalicAngle 0 /Ascent 900 /Descent -200 \
              /CapHeight 700 /StemV 80 /FontFile2 22 0 R >>"
                .to_vec(),
        ),
        (22, stream_obj(&format!("/Length1 {}", ff.len()), &ff)),
    ]
}

/// One page: `content`, extra resources `res`, extra page entries,
/// with or without a `/MediaBox`, plus `extra` objects (later
/// definitions of an object number are written too; the
/// cross-reference table names the first).
fn doc(
    content: &str,
    res: &str,
    page_extra: &str,
    mediabox: bool,
    extra: Vec<(u32, Vec<u8>)>,
) -> Vec<u8> {
    let mb = if mediabox {
        "/MediaBox [0 0 200 200]"
    } else {
        ""
    };
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R {mb} /Resources << /Font << /F1 20 0 R >> {res} >> \
                 /Contents 4 0 R {page_extra} >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content.as_bytes())),
    ];
    objs.extend(font_objs());
    objs.extend(extra);
    build_pdf(&objs)
}

fn page(content: &str) -> Vec<u8> {
    doc(content, "", "", true, vec![])
}

/// The same bytes with `startxref` pointing past the end: the strict
/// parse fails, recovery rebuilds the table by scanning.
fn broken_xref(mut pdf: Vec<u8>) -> Vec<u8> {
    let i = pdf.windows(10).rposition(|w| w == b"startxref\n").unwrap() + 10;
    pdf.truncate(i);
    pdf.extend_from_slice(b"999999\n%%EOF\n");
    pdf
}

fn image_xobject(dict: &str, data: &[u8]) -> Vec<(u32, Vec<u8>)> {
    vec![(
        30,
        stream_obj(
            &format!("/Type /XObject /Subtype /Image /ColorSpace /DeviceGray {dict}"),
            data,
        ),
    )]
}

fn image_page(dict: &str, data: impl Fn(u32) -> Vec<u8>) -> impl Fn(u32) -> Vec<u8> {
    let dict = dict.to_string();
    move |v| {
        doc(
            &format!("q 40 0 0 40 80 80 cm /Im Do Q {KEPT}"),
            "/XObject << /Im 30 0 R >>",
            "",
            true,
            image_xobject(&dict, &data(v)),
        )
    }
}

fn form_page(form_dict: &str, body: impl Fn(u32) -> String) -> impl Fn(u32) -> Vec<u8> {
    let form_dict = form_dict.to_string();
    move |v| {
        doc(
            &format!("/Fm Do {KEPT}"),
            "/XObject << /Fm 30 0 R >>",
            "",
            true,
            vec![(
                30,
                stream_obj(
                    &format!("/Type /XObject /Subtype /Form /Resources << /Font << /F1 20 0 R >> >> {form_dict}"),
                    body(v).as_bytes(),
                ),
            )],
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Want {
    /// Refused, the message containing this text.
    Refused(&'static str),
    /// Redacted (see the module oracle).
    Redacted,
}

struct Mode {
    /// The document opens (`None`: it does not, with this text in the
    /// error).
    opens: Result<(), &'static str>,
    /// Page 1 renders.
    renders: bool,
    /// Recovery rules recorded while opening and rendering (strict:
    /// always none).
    rules: &'static [RecoveryRule],
    redact: Want,
}

const fn strict(renders: bool, redact: Want) -> Mode {
    Mode {
        opens: Ok(()),
        renders,
        rules: &[],
        redact,
    }
}

const fn recovery(renders: bool, rules: &'static [RecoveryRule], redact: Want) -> Mode {
    Mode {
        opens: Ok(()),
        renders,
        rules,
        redact,
    }
}

const fn no_open(cause: &'static str) -> Mode {
    Mode {
        opens: Err(cause),
        renders: false,
        rules: &[],
        redact: Want::Refused(""),
    }
}

use RecoveryRule as R;
use Want::{Redacted, Refused};

const UNCERTAIN: &str = "region removal on a page with uncertain content";

fn open(pdf: &[u8], recovering: bool) -> Result<Document, Error> {
    if recovering {
        Document::from_bytes_recovering(pdf)
    } else {
        Document::from_bytes(pdf)
    }
}

fn page_text(d: &Document) -> String {
    extract_text(d, &d.pages()[0], &TextOptions::default())
        .unwrap()
        .chars
        .iter()
        .filter_map(|c| c.unicode.clone())
        .collect()
}

fn rules(d: &Document) -> Vec<RecoveryRule> {
    let mut r: Vec<RecoveryRule> = d
        .take_warnings()
        .into_iter()
        .filter_map(|w| match w {
            Warning::RecoveryApplied { rule, .. } => Some(rule),
            _ => None,
        })
        .collect();
    r.sort_by_key(|x| format!("{x:?}"));
    r.dedup();
    r
}

/// The refusal message without the glyph code it names.
fn mask(e: &Error) -> String {
    let s = e.to_string();
    match s.find("glyph 0x") {
        Some(i) => {
            let j = s[i + 8..].find(' ').map_or(s.len(), |k| i + 8 + k);
            format!("{}glyph{}", &s[..i], &s[j..])
        }
        None => s,
    }
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// Check one mode of one case; returns the output on a redaction.
fn check_mode(
    name: &str,
    make: &dyn Fn(u32) -> Vec<u8>,
    recovering: bool,
    want: &Mode,
) -> Option<Vec<u8>> {
    let tag = format!(
        "{name} ({})",
        if recovering { "recovery" } else { "strict" }
    );
    let (a, b) = (make(1), make(2));
    assert_ne!(a, b, "{tag}: the variants do not differ");
    let (da, db) = (open(&a, recovering), open(&b, recovering));
    let (da, db) = match (want.opens, da, db) {
        (Ok(()), Ok(x), Ok(y)) => (x, y),
        (Err(cause), Err(x), Err(y)) => {
            assert_eq!(x.to_string(), y.to_string(), "{tag}: open refused unlike");
            assert!(x.to_string().contains(cause), "{tag}: open error {x}");
            for s in [secret(1), secret(2)] {
                assert!(
                    !x.to_string().contains(s),
                    "{tag}: the error shows the secret"
                );
            }
            return None;
        }
        (w, x, y) => panic!(
            "{tag}: open expected {w:?}, got {:?} / {:?}",
            x.map(|_| ()).map_err(|e| e.to_string()),
            y.map(|_| ()).map_err(|e| e.to_string())
        ),
    };
    assert_eq!(da.recovery_mode(), recovering, "{tag}: recovery mode");
    let rendered = render_page(&da, &da.pages()[0], &RenderOptions::default());
    assert_eq!(
        rendered.is_ok(),
        want.renders,
        "{tag}: render {:?}",
        rendered.map(|_| ()).map_err(|e| e.to_string())
    );
    let got = rules(&da);
    let mut expected = want.rules.to_vec();
    expected.sort_by_key(|x| format!("{x:?}"));
    assert_eq!(got, expected, "{tag}: recovery rules recorded");
    let _ = db.take_warnings();
    let (ra, rb) = (
        redact(&da, &[(0, vec![RECT])], &RedactOptions::new()),
        redact(&db, &[(0, vec![RECT])], &RedactOptions::new()),
    );
    match (want.redact, ra, rb) {
        (Refused(cause), Err(x), Err(y)) => {
            // A refusal names the first glyph refused, as everywhere in
            // the redaction tests (the message goes to the caller, who
            // holds the original; it never reaches an output).
            assert_eq!(mask(&x), mask(&y), "{tag}: refused unlike");
            assert!(
                x.to_string().contains(cause),
                "{tag}: refused for another cause: {x}"
            );
            for s in [secret(1), secret(2)] {
                assert!(
                    !x.to_string().contains(s),
                    "{tag}: the refusal shows the secret"
                );
            }
            None
        }
        (Redacted, Ok(x), Ok(y)) => {
            assert!(
                x.output == y.output,
                "{tag}: the outputs differ — the secret reached the output"
            );
            // The output parses strictly. It renders in the mode the
            // input was opened in, and needs no recovery rule the input
            // did not: what the redaction kept (an annotation outside
            // the rect, say) may keep its own fault, nothing is added.
            Document::from_bytes(&x.output)
                .unwrap_or_else(|e| panic!("{tag}: the output does not parse strictly: {e}"));
            let out = open(&x.output, recovering).unwrap();
            let r = render_page(&out, &out.pages()[0], &RenderOptions::default());
            if want.renders {
                r.unwrap_or_else(|e| panic!("{tag}: the output does not render: {e}"));
            }
            let added: Vec<_> = rules(&out)
                .into_iter()
                .filter(|r| !got.contains(r))
                .collect();
            assert!(
                added.is_empty(),
                "{tag}: the output needs recovery the input did not: {added:?}"
            );
            let t = page_text(&out);
            assert!(
                t.contains("PUBLICWORD"),
                "{tag}: lost the public text outside the rect"
            );
            for s in [secret(1), secret(2)] {
                assert!(!t.contains(s), "{tag}: the secret is still drawn");
                assert!(
                    !contains(&x.output, s),
                    "{tag}: the output holds the secret's bytes"
                );
            }
            Some(x.output)
        }
        (w, x, y) => panic!(
            "{tag}: redaction expected {w:?}, got {:?} / {:?}",
            x.map(|r| r.output.len()).map_err(|e| e.to_string()),
            y.map(|r| r.output.len()).map_err(|e| e.to_string())
        ),
    }
}

fn case(name: &str, make: impl Fn(u32) -> Vec<u8>, s: Mode, r: Mode) {
    check_mode(name, &make, false, &s);
    check_mode(name, &make, true, &r);
}

// ---- 1. content streams that do not parse ----------------------------

#[test]
fn content_stream_that_does_not_parse() {
    // A literal string that never ends: the stream is only partly
    // parsable, and no mode guesses where the string stops.
    case(
        "unterminated string",
        |v| {
            page(&format!(
                "{KEPT} BT /F1 10 Tf 80 100 Td ({} Tj ET",
                secret(v)
            ))
        },
        strict(false, Refused("while reading literal string")),
        recovery(false, &[], Refused("while reading literal string")),
    );
    case(
        "unbalanced array",
        |v| page(&format!("] {} {KEPT}", shown(v))),
        strict(false, Refused("")),
        recovery(false, &[], Refused("")),
    );
}

// ---- 2. unknown operators ---------------------------------------------

#[test]
fn unknown_operator() {
    // Recovery drops it (the renderer draws the rest); the redaction
    // walker keeps it as an unknown operation and refuses the page —
    // it cannot know what the operator would have drawn.
    for (name, content) in [
        ("before the text", format!("zork {}", "{S}")),
        (
            "inside the text object",
            "BT /F1 10 Tf 80 100 Td 1 2 zork (SECRET) Tj ET".into(),
        ),
    ] {
        case(
            &format!("unknown operator {name}"),
            |v| {
                let c = content
                    .replace("{S}", &shown(v))
                    .replace("SECRET", secret(v));
                page(&format!("{c} {KEPT}"))
            },
            strict(false, Refused("unknown operator 'zork'")),
            recovery(true, &[R::IgnoredUnknownOperator], Refused(UNCERTAIN)),
        );
    }
    // Inside BX … EX it is valid in both modes (§7.8.2) and still
    // refused: what it draws is unknown.
    case(
        "unknown operator inside BX/EX",
        |v| page(&format!("BX zork EX {} {KEPT}", shown(v))),
        strict(true, Refused(UNCERTAIN)),
        recovery(true, &[], Refused(UNCERTAIN)),
    );
}

// ---- 3. operand counts --------------------------------------------------

#[test]
fn wrong_operand_count() {
    case(
        "re with three operands",
        |v| page(&format!("0 0 10 re f {} {KEPT}", shown(v))),
        strict(false, Refused("expects 4 operand(s), found 3")),
        recovery(false, &[], Refused("expects 4 operand(s), found 3")),
    );
}

// ---- 4. graphics state ---------------------------------------------------

#[test]
fn invalid_graphics_state() {
    // A Q without a saved state: every major reader ignores it, the
    // walker sees all of the page, the output is written without it.
    case(
        "stray Q",
        |v| page(&format!("Q {} {KEPT}", shown(v))),
        strict(false, Refused("'Q' without matching 'q'")),
        recovery(true, &[R::IgnoredRestoreWithoutSave], Redacted),
    );
    case(
        "stray Q after a balanced pair",
        |v| page(&format!("q 1 0 0 1 0 0 cm Q Q {} {KEPT}", shown(v))),
        strict(false, Refused("'Q' without matching 'q'")),
        recovery(true, &[R::IgnoredRestoreWithoutSave], Redacted),
    );
    // A state operator inside a path object: recovery applies it before
    // painting; the redaction's own walk refuses the construct.
    for op in ["1 0 0 RG", "2 0 0 2 0 0 cm"] {
        case(
            &format!("{op} inside a path object"),
            |v| page(&format!("70 90 m {op} 130 90 l S {} {KEPT}", shown(v))),
            strict(false, Refused("is not allowed inside a path object")),
            recovery(
                true,
                &[R::OperatorInsidePathObject],
                Refused("is not allowed inside a path object"),
            ),
        );
    }
    case(
        "undefined ExtGState name",
        |v| page(&format!("/GS9 gs {} {KEPT}", shown(v))),
        strict(false, Refused("region removal on a page with blockers")),
        recovery(
            false,
            &[],
            Refused("region removal on a page with blockers"),
        ),
    );
    // An image /SMask that is not a stream: recovery draws the image
    // unmasked; whether it is masked is not known, so it is refused.
    case(
        "image /SMask /None",
        image_page("/Width 2 /Height 2 /BitsPerComponent 8 /SMask /None", |v| {
            vec![v as u8, 2, 3, 4]
        }),
        strict(false, Refused("image soft mask")),
        recovery(
            true,
            &[R::IgnoredInvalidSoftMask],
            Refused("image soft mask"),
        ),
    );
}

// ---- 5–6. images: sizes, decoders ------------------------------------

#[test]
fn invalid_images_and_decoders() {
    let both = |name: &str, dict: &str, data: fn(u32) -> Vec<u8>, cause: &'static str| {
        case(
            name,
            image_page(dict, data),
            strict(false, Refused(cause)),
            recovery(false, &[], Refused(cause)),
        );
    };
    both(
        "image data shorter than its size",
        "/Width 20 /Height 20 /BitsPerComponent 8",
        |v| vec![v as u8; 10],
        "data length 10 does not match",
    );
    both(
        "image without /Width",
        "/Height 2 /BitsPerComponent 8",
        |v| vec![v as u8, 2, 3, 4],
        "image has no /Width",
    );
    both(
        "JPEG with a truncated marker structure",
        "/Width 2 /Height 2 /BitsPerComponent 8 /Filter /DCTDecode",
        |v| vec![0xff, 0xd8, 0xff, v as u8, 1, 2, 3],
        "JPEG marker structure",
    );
    both(
        "JBIG2 with an unknown segment",
        "/Width 2 /Height 2 /BitsPerComponent 1 /Filter /JBIG2Decode",
        |v| vec![0, 0, 0, v as u8, 1, 2, 3],
        "JBIG2Decode",
    );
    both(
        "Flate image data that fails part-way",
        "/Width 2 /Height 2 /BitsPerComponent 8 /Filter /FlateDecode",
        |v| vec![0x78, 0x9c, 0xff, v as u8, 1, 2, 3],
        "FlateDecode failed",
    );
}

// ---- 7–8. page tree, cross-reference table -------------------------------

fn tree(kids: &str, extra: Vec<(u32, Vec<u8>)>, v: u32) -> Vec<u8> {
    let mut o = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, format!("<< /Type /Pages /Kids [{kids}] /Count 2 >>").into_bytes()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 20 0 R >> >> \
              /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", format!("{} {KEPT}", shown(v)).as_bytes())),
    ];
    o.extend(font_objs());
    o.extend(extra);
    build_pdf(&o)
}

#[test]
fn invalid_page_tree() {
    // Recovery rebuilds the cross-reference table, not the page tree:
    // neither mode opens these, so there is nothing to redact.
    case(
        "page tree kid that is not a page",
        |v| tree("3 0 R 7 0 R", vec![(7, b"<< /Type /Font >>".to_vec())], v),
        no_open("unexpected /Type /Font"),
        no_open("unexpected /Type /Font"),
    );
    case(
        "page tree cycle",
        |v| tree("3 0 R 2 0 R", vec![], v),
        no_open("cycle detected in page tree"),
        no_open("cycle detected in page tree"),
    );
}

#[test]
fn broken_cross_reference_table() {
    case(
        "startxref past the end",
        |v| broken_xref(page(&format!("{} {KEPT}", shown(v)))),
        no_open("is beyond end of input"),
        recovery(true, &[], Redacted),
    );
    // Two definitions of the content stream. With a valid table every
    // reader uses the first; after a rebuild Poppler, MuPDF, qpdf and
    // kasane use the last. Either way the unused definition must not
    // reach the output: the secret is put in one definition only, once
    // in each.
    let other = format!("BT /F1 10 Tf 10 180 Td (OTHER) Tj ET {KEPT}");
    let second = |c: &str| vec![(4, stream_obj("", c.as_bytes()))];
    for (name, first_secret, broken) in [
        ("secret in the first definition, valid table", true, false),
        ("secret in the second definition, valid table", false, false),
        ("secret in the first definition, rebuilt table", true, true),
        (
            "secret in the second definition, rebuilt table",
            false,
            true,
        ),
    ] {
        let make = |v: u32| {
            let s = format!("{} {KEPT}", shown(v));
            let pdf = if first_secret {
                doc(&s, "", "", true, second(&other))
            } else {
                doc(&other, "", "", true, second(&s))
            };
            if broken {
                broken_xref(pdf)
            } else {
                pdf
            }
        };
        let s = if broken {
            no_open("is beyond end of input")
        } else {
            strict(true, Redacted)
        };
        case(name, make, s, recovery(true, &[], Redacted));
    }
}

// ---- 9. page boxes ------------------------------------------------------

#[test]
fn missing_media_box() {
    // Recovery draws the page on US Letter; the page box is inferred,
    // so the redaction refuses.
    case(
        "page without /MediaBox",
        |v| doc(&format!("{} {KEPT}", shown(v)), "", "", false, vec![]),
        no_open("page 1 has no MediaBox"),
        recovery(true, &[R::MediaBoxDefault], Refused("has no MediaBox")),
    );
}

// ---- 10. annotations ------------------------------------------------------

#[test]
fn invalid_annotations() {
    let on_page = |v: u32, annots: &str, extra: Vec<(u32, Vec<u8>)>| {
        doc(&format!("{} {KEPT}", shown(v)), "", annots, true, extra)
    };
    case(
        "/Annots entry that is not a dictionary",
        |v| on_page(v, "/Annots [5]", vec![]),
        strict(false, Refused("/Annots[0] is not a dictionary")),
        recovery(
            true,
            &[R::IgnoredInvalidAnnotation],
            Refused("/Annots[0] is not a dictionary"),
        ),
    );
    let square = |rect: &str| {
        vec![
            (30, format!("<< /Type /Annot /Subtype /Square /Rect [{rect}] /AS /On /AP << /N << /On 31 0 R >> >> >>").into_bytes()),
            (31, b"<< /Foo 1 >>".to_vec()),
        ]
    };
    // Outside the rect the annotation is kept as it is: the walker
    // needs nothing of it.
    case(
        "appearance state that is not a stream, outside the rect",
        |v| on_page(v, "/Annots [30 0 R]", square("10 150 50 190")),
        strict(false, Redacted),
        recovery(true, &[R::IgnoredInvalidAnnotation], Redacted),
    );
    case(
        "appearance state that is not a stream, meeting the rect",
        |v| on_page(v, "/Annots [30 0 R]", square("70 90 130 110")),
        strict(
            false,
            Refused("redaction of an annotation other than a link"),
        ),
        recovery(
            true,
            &[R::IgnoredInvalidAnnotation],
            Refused("redaction of an annotation other than a link"),
        ),
    );
    // The secret in the text of a FreeText annotation whose appearance
    // recovery skips: a viewer may draw /Contents itself.
    case(
        "FreeText with an invalid appearance and the secret in /Contents",
        |v| {
            doc(
                KEPT,
                "",
                "/Annots [30 0 R]",
                true,
                vec![
                    (
                        30,
                        format!(
                            "<< /Type /Annot /Subtype /FreeText /Rect [70 90 130 110] /DA (/Helv 9 Tf 0 g) \
                             /Contents ({}) /AP << /N 31 0 R >> >>",
                            secret(v)
                        )
                        .into_bytes(),
                    ),
                    (31, b"<< /Foo 1 >>".to_vec()),
                ],
            )
        },
        strict(
            false,
            Refused("redaction of an annotation other than a link"),
        ),
        recovery(
            true,
            &[R::IgnoredInvalidAnnotation],
            Refused("redaction of an annotation other than a link"),
        ),
    );
}

// ---- 11. form XObjects ------------------------------------------------------

#[test]
fn invalid_form_xobjects() {
    case(
        "form without /BBox",
        form_page("", shown),
        strict(false, Refused("form XObject has no /BBox")),
        recovery(false, &[], Refused("form XObject has no /BBox")),
    );
    case(
        "unknown operator inside a form",
        form_page("/BBox [0 0 200 200]", |v| format!("zork {}", shown(v))),
        strict(false, Refused("unknown operator 'zork'")),
        recovery(true, &[R::IgnoredUnknownOperator], Refused(UNCERTAIN)),
    );
    case(
        "stray Q inside a form",
        form_page("/BBox [0 0 200 200]", |v| format!("Q {}", shown(v))),
        strict(false, Refused("'Q' without matching 'q'")),
        recovery(true, &[R::IgnoredRestoreWithoutSave], Redacted),
    );
}

// ---- fonts, marked content, encodings, encryption -------------------------

#[test]
fn recovered_resources() {
    // A font resource that is not defined: recovery draws Helvetica;
    // which glyphs another reader shows is unknown.
    case(
        "undefined font resource",
        |v| {
            page(&format!(
                "BT /F9 10 Tf 80 100 Td ({}) Tj ET {KEPT}",
                secret(v)
            ))
        },
        strict(false, Refused("/Font /F9 is not defined")),
        recovery(true, &[R::DefaultFontSubstituted], Refused(UNCERTAIN)),
    );
    case(
        "BDC with an undefined /Properties name",
        |v| page(&format!("/OC /P9 BDC {} EMC {KEPT}", shown(v))),
        strict(false, Refused("/Properties entry /P9")),
        recovery(
            true,
            &[R::IgnoredUndefinedProperty],
            Refused("/Properties entry /P9"),
        ),
    );
    // A /Differences entry that is neither an integer nor a name, in a
    // font F2. Recovery skips the entry. Used only inside the rect, the
    // font goes with the text it drew (every code is removed whatever
    // glyph it maps to); used outside the rect as well, the output would
    // keep the malformed font, and the strict check of the output
    // refuses it.
    let differences = |public_in_f2: bool| {
        move |v: u32| {
            let w = vec!["600"; 95].join(" ");
            let kept = if public_in_f2 {
                KEPT.replace("/F1", "/F2")
            } else {
                KEPT.to_string()
            };
            let mut o = vec![
                (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Resources << /Font << /F1 20 0 R /F2 23 0 R >> >> /Contents 4 0 R >>"
                        .to_vec(),
                ),
                (
                    4,
                    stream_obj(
                        "",
                        format!("BT /F2 10 Tf 80 100 Td ({}) Tj ET {kept}", secret(v)).as_bytes(),
                    ),
                ),
                (
                    23,
                    format!(
                        "<< /Type /Font /Subtype /TrueType /BaseFont /LiberationSans /FirstChar 32 \
                         /LastChar 126 /Widths [{w}] /Encoding << /Type /Encoding \
                         /BaseEncoding /WinAnsiEncoding /Differences [65 (x) /A] >> \
                         /FontDescriptor 21 0 R >>"
                    )
                    .into_bytes(),
                ),
            ];
            o.extend(font_objs());
            build_pdf(&o)
        }
    };
    let bad = "/Differences entries must be integers or names";
    case(
        "/Differences with a string entry, font used inside the rect only",
        differences(false),
        strict(false, Refused(bad)),
        recovery(true, &[R::IgnoredInvalidEncodingEntry], Redacted),
    );
    case(
        "/Differences with a string entry, font used outside the rect too",
        differences(true),
        strict(false, Refused(bad)),
        recovery(true, &[R::IgnoredInvalidEncodingEntry], Refused(bad)),
    );
}

#[test]
fn recovered_encryption_key_is_not_redacted() {
    // A revision-4 key shorter than 128 bits, zero-padded as Acrobat
    // does: only recovery decrypts it, and no writer accepts an
    // encrypted document.
    let pdf = std::fs::read(common::fixture("encrypted/enc_r4_rc4_40_acrobat.pdf")).unwrap();
    assert!(Document::from_bytes(&pdf).is_err());
    let d = Document::open(&pdf, &OpenOptions::new().with_recovery(true)).unwrap();
    assert!(rules(&d).contains(&R::PaddedShortEncryptionKey));
    // Its text is in a non-embedded font, which region removal refuses
    // first (whatever the rect); behind it the writer refuses the
    // encryption.
    for rect in [RECT, (1.0, 1.0, 2.0, 2.0)] {
        let e = redact(&d, &[(0, vec![rect])], &RedactOptions::new()).unwrap_err();
        assert!(e.to_string().contains("non-embedded font"), "{e}");
    }
    let e = kasane_pdf::rewrite(&d).unwrap_err();
    assert!(
        e.to_string().contains("writing an encrypted document"),
        "{e}"
    );
}

// ---- 12–14. several faults, recovery only, valid input ---------------------

#[test]
fn several_faults() {
    case(
        "no /MediaBox and a stray Q",
        |v| doc(&format!("Q {} {KEPT}", shown(v)), "", "", false, vec![]),
        no_open("page 1 has no MediaBox"),
        recovery(
            true,
            &[R::MediaBoxDefault, R::IgnoredRestoreWithoutSave],
            Refused("has no MediaBox"),
        ),
    );
    case(
        "stray Q and an unknown operator",
        |v| page(&format!("Q {} zork {KEPT}", shown(v))),
        strict(false, Refused("'Q' without matching 'q'")),
        recovery(
            true,
            &[R::IgnoredRestoreWithoutSave, R::IgnoredUnknownOperator],
            Refused(UNCERTAIN),
        ),
    );
    case(
        "stray Q and a rebuilt cross-reference table",
        |v| broken_xref(page(&format!("Q {} {KEPT}", shown(v)))),
        no_open("is beyond end of input"),
        recovery(true, &[R::IgnoredRestoreWithoutSave], Redacted),
    );
}

#[test]
fn valid_document_redacts_identically_in_both_modes() {
    let make = |v: u32| page(&format!("{} {KEPT}", shown(v)));
    let s = check_mode("valid", &make, false, &strict(true, Redacted)).unwrap();
    let r = check_mode("valid", &make, true, &recovery(true, &[], Redacted)).unwrap();
    assert!(
        s == r,
        "a conforming document redacts differently once opened in recovery mode"
    );
}

// ---- fail-closed paths -------------------------------------------------

#[test]
fn limits_and_rewrite_failures_return_no_output() {
    // Limits reached while walking the page and while writing the
    // output: an error, and with it no bytes. Recovery mode runs under
    // the same limits.
    let make = |v: u32| page(&format!("{} {KEPT}", shown(v)));
    let stray = |v: u32| page(&format!("Q {} {KEPT}", shown(v)));
    for (what, limits) in [
        ("max_operations", Limits::default().with_max_operations(3)),
        (
            "max_output_bytes",
            Limits::default().with_max_output_bytes(2000),
        ),
        (
            "max_output_objects",
            Limits::default().with_max_output_objects(2),
        ),
    ] {
        for (recovering, pdf) in [
            (false, make(1)),
            (true, stray(1)),
            (true, broken_xref(make(1))),
        ] {
            let d = open(&pdf, recovering).unwrap();
            let r = redact(
                &d,
                &[(0, vec![RECT])],
                &RedactOptions::new().with_limits(limits.clone()),
            );
            match r {
                Err(Error::LimitExceeded { limit, .. }) => assert_eq!(limit, what),
                other => panic!(
                    "{what} (recovery {recovering}): {:?}",
                    other.map(|r| r.output.len()).map_err(|e| e.to_string())
                ),
            }
        }
    }
}

#[test]
fn unresolvable_reference_is_refused() {
    // /Im names an object that does not exist (null, §7.3.10).
    case(
        "XObject reference to a missing object",
        |v| {
            doc(
                &format!("{} /Im Do {KEPT}", shown(v)),
                "/XObject << /Im 99 0 R >>",
                "",
                true,
                vec![],
            )
        },
        strict(false, Refused("/XObject /Im is not a stream")),
        recovery(false, &[], Refused("/XObject /Im is not a stream")),
    );
}

#[test]
fn cli_refusal_writes_no_file() {
    // The CLI opens strictly: on a refusal neither the output nor its
    // temporary file exists, and the message carries no secret.
    let dir = common::tmpdir("redact-recovery-cli");
    for (name, pdf) in [
        ("unknown", page(&format!("zork {} {KEPT}", shown(1)))),
        ("stray-q", page(&format!("Q {} {KEPT}", shown(1)))),
        ("xref", broken_xref(page(&format!("{} {KEPT}", shown(1))))),
        (
            "mediabox",
            doc(&format!("{} {KEPT}", shown(1)), "", "", false, vec![]),
        ),
    ] {
        let input = dir.join(format!("{name}.pdf"));
        let output = dir.join(format!("{name}-out.pdf"));
        std::fs::write(&input, &pdf).unwrap();
        let o = common::bin()
            .args([
                "redact",
                input.to_str().unwrap(),
                "--page",
                "1",
                "--rect",
                "60",
                "60",
                "140",
                "140",
                "--output",
            ])
            .arg(&output)
            .output()
            .unwrap();
        assert!(
            !o.status.success(),
            "{name}: the CLI redacted a malformed file"
        );
        assert!(!output.exists(), "{name}: an output file was written");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("kasane-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{name}: a temporary file was left");
        assert!(
            !String::from_utf8_lossy(&o.stderr).contains(secret(1)),
            "{name}: the message shows the secret"
        );
    }
}
