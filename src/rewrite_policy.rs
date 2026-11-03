//! [`rewrite_with`]: the full rewrite under explicit policies, with a
//! report of the executable content it carried
//! (`docs/plans/rewrite-policy.md` §2, §3).
//!
//! With [`RewriteOptions::default()`] the bytes and the errors are
//! exactly [`rewrite`](crate::rewrite)'s: the same pipeline runs, and the
//! report is taken afterwards from objects it already read.
//!
//! [`ExecutableContentPolicy::Carry`] is **not sanitizing**: it copies
//! scripts and actions byte for byte, executes nothing, and the output
//! can do whatever the input could.
//!
//! Reports hold classes, kinds, positions, object numbers and lengths —
//! never script text, URLs, field names, name-tree keys or digests.

use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::IndirectRef;
use std::fmt;

/// What to do with executable content (actions and scripts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ExecutableContentPolicy {
    /// The 0.4 profile, unchanged: the document-level entry points —
    /// catalog `/AA`, a JavaScript `/OpenAction`, `/Names /JavaScript` —
    /// are refused, everything else is carried. Unlike
    /// [`rewrite`](crate::rewrite), what is carried is reported.
    #[default]
    Profile,
    /// Everything [`Profile`](Self::Profile) refuses, and every
    /// [`Script`](ActionClass::Script), [`External`](ActionClass::External)
    /// and [`Unknown`](ActionClass::Unknown) action anywhere — page,
    /// annotation, field, outline, `/Next` chain, name tree — as well as
    /// XFA scripts and any structure that could not be classified.
    /// Navigation, state and multimedia actions are carried and
    /// reported.
    RefuseScriptsAndExternal,
    /// Carry every script and action verbatim, never executed, all
    /// reported. Not sanitizing: the output can run whatever the input
    /// could. Unknown actions and structures that could not be classified
    /// are refused.
    Carry,
}

/// What to do with multimedia.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum MultimediaPolicy {
    /// The catalog `/RichMedia` is refused (as by
    /// [`rewrite`](crate::rewrite)); page-level multimedia annotations and
    /// multimedia actions are carried and reported. The only policy in
    /// 0.5: carrying `/RichMedia` needs its own design.
    #[default]
    Refuse,
}

/// What to do with usage-rights signatures (`/Perms /UR`, `/UR3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum UsageRightsPolicy {
    /// A document with a usage-rights signature is refused (as by
    /// [`rewrite`](crate::rewrite)). The only policy so far; removing
    /// them is planned and not available.
    #[default]
    Refuse,
}

/// Options of [`rewrite_with`]. Every default is
/// [`rewrite`](crate::rewrite)'s behaviour.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RewriteOptions {
    /// Default: [`ExecutableContentPolicy::Profile`].
    pub executable_content: ExecutableContentPolicy,
    /// Default: [`MultimediaPolicy::Refuse`].
    pub multimedia: MultimediaPolicy,
    /// Default: [`UsageRightsPolicy::Refuse`].
    pub usage_rights: UsageRightsPolicy,
    /// The output limits (traversal budget, emitted bytes, walk depth);
    /// default: [`Limits::default()`].
    pub limits: Limits,
}

impl RewriteOptions {
    /// The defaults: [`rewrite`](crate::rewrite)'s behaviour.
    pub fn new() -> Self {
        Self::default()
    }

    /// With the executable-content policy `p`.
    pub fn with_executable_content(mut self, p: ExecutableContentPolicy) -> Self {
        self.executable_content = p;
        self
    }

    /// With the multimedia policy `p`.
    pub fn with_multimedia(mut self, p: MultimediaPolicy) -> Self {
        self.multimedia = p;
        self
    }

    /// With the usage-rights policy `p`.
    pub fn with_usage_rights(mut self, p: UsageRightsPolicy) -> Self {
        self.usage_rights = p;
        self
    }

    /// With the output limits `l`.
    pub fn with_limits(mut self, l: Limits) -> Self {
        self.limits = l;
        self
    }
}

/// The rewritten file and what the rewrite carried.
#[must_use]
#[derive(Debug)]
pub struct RewriteOutput {
    bytes: Vec<u8>,
    report: RewriteReport,
}

impl RewriteOutput {
    /// The complete file.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The complete file, the report dropped.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// What the rewrite carried.
    pub fn report(&self) -> &RewriteReport {
        &self.report
    }

    /// The file and the report.
    pub fn into_parts(self) -> (Vec<u8>, RewriteReport) {
        (self.bytes, self.report)
    }
}

/// What a successful [`rewrite_with`] carried. A refusal is an `Err`, so
/// every listed item was carried. Content-free.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RewriteReport {
    pub(crate) executable_content: ExecutableContentPolicy,
    pub(crate) multimedia: MultimediaPolicy,
    pub(crate) usage_rights: UsageRightsPolicy,
    pub(crate) items: Vec<ExecutableItem>,
    pub(crate) unclassified: Vec<UnclassifiedStructure>,
    pub(crate) version: VersionChange,
}

impl RewriteReport {
    /// The executable-content policy applied.
    pub fn executable_content_policy(&self) -> ExecutableContentPolicy {
        self.executable_content
    }

    /// The multimedia policy applied.
    pub fn multimedia_policy(&self) -> MultimediaPolicy {
        self.multimedia
    }

    /// The usage-rights policy applied.
    pub fn usage_rights_policy(&self) -> UsageRightsPolicy {
        self.usage_rights
    }

    /// Every action, multimedia annotation and XFA script carried, in a
    /// fixed order: the catalog `/OpenAction`, catalog `/AA` triggers,
    /// then each page (its `/AA`, then each annotation: the annotation
    /// itself when it is multimedia, its `/A`, its `/AA`), field `/AA`
    /// (field nodes that are not a page's annotation), outline `/A`,
    /// `/Names /JavaScript` entries, XFA scripts. Each owner's `/Next`
    /// chain follows its action depth first.
    pub fn executable_content(&self) -> &[ExecutableItem] {
        &self.items
    }

    /// Counts per class.
    pub fn executable_summary(&self) -> ExecutableSummary {
        let mut s = ExecutableSummary::default();
        for it in &self.items {
            let n = match it.class {
                ActionClass::Script => &mut s.script,
                ActionClass::External => &mut s.external,
                ActionClass::State => &mut s.state,
                ActionClass::Navigation => &mut s.navigation,
                ActionClass::Multimedia => &mut s.multimedia,
                ActionClass::Unknown => &mut s.unknown,
            };
            *n += 1;
        }
        s
    }

    /// Structures the classification could not follow to the end (a
    /// cycle in an outline, field or name tree, an action chain past the
    /// walk depth). Only [`ExecutableContentPolicy::Profile`] carries
    /// such a document (as [`rewrite`](crate::rewrite) does); the other
    /// policies refuse it.
    pub fn unclassified(&self) -> &[UnclassifiedStructure] {
        &self.unclassified
    }

    /// The version declared by the input and the one written.
    pub fn version(&self) -> VersionChange {
        self.version
    }
}

impl fmt::Display for RewriteReport {
    /// Counts only.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.executable_summary();
        write!(
            f,
            "{} executable item(s): {} script, {} external, {} state, {} navigation, {} multimedia, \
             {} unknown; {} unclassified structure(s)",
            self.items.len(),
            s.script,
            s.external,
            s.state,
            s.navigation,
            s.multimedia,
            s.unknown,
            self.unclassified.len()
        )
    }
}

/// Counts of [`ExecutableItem`]s per [`ActionClass`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct ExecutableSummary {
    /// [`ActionClass::Script`].
    pub script: usize,
    /// [`ActionClass::External`].
    pub external: usize,
    /// [`ActionClass::State`].
    pub state: usize,
    /// [`ActionClass::Navigation`].
    pub navigation: usize,
    /// [`ActionClass::Multimedia`].
    pub multimedia: usize,
    /// [`ActionClass::Unknown`].
    pub unknown: usize,
}

/// Actions by effect (ISO 32000-2 §12.6.4, §13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ActionClass {
    /// Runs code in the viewer: JavaScript actions, XFA scripts.
    Script,
    /// Opens files or applications, accesses the network, sends form
    /// data: Launch, URI, GoToR, GoToE, SubmitForm, ImportData.
    External,
    /// Changes the document's state: ResetForm, Hide, SetOCGState,
    /// Trans, Named other than page navigation.
    State,
    /// Moves the view: GoTo, GoToDp, Thread, GoTo3DView, Named
    /// NextPage / PrevPage / FirstPage / LastPage.
    Navigation,
    /// Plays media: Rendition, Movie, Sound, RichMediaExecute; RichMedia,
    /// Screen, Movie, Sound and 3D annotations.
    Multimedia,
    /// An `/S` not in ISO 32000-2, a missing `/S`, or an action value
    /// that is not a dictionary.
    Unknown,
}

impl ActionClass {
    /// Lower-case name (`script`, `external`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            ActionClass::Script => "script",
            ActionClass::External => "external",
            ActionClass::State => "state",
            ActionClass::Navigation => "navigation",
            ActionClass::Multimedia => "multimedia",
            ActionClass::Unknown => "unknown",
        }
    }
}

/// The kind of an [`ExecutableItem`]: the action type (`/S`), the
/// multimedia annotation subtype, or an XFA script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum ActionKind {
    JavaScript,
    Launch,
    Uri,
    GoToR,
    GoToE,
    SubmitForm,
    ImportData,
    ResetForm,
    Hide,
    SetOcgState,
    Trans,
    /// `/S /Named`; navigation for the four page operations, state
    /// otherwise.
    Named,
    GoTo,
    GoToDp,
    Thread,
    GoTo3DView,
    Rendition,
    Movie,
    Sound,
    RichMediaExecute,
    /// A multimedia annotation (`/Subtype` RichMedia, Screen, Movie,
    /// Sound, 3D) — the annotation itself, not an action.
    MultimediaAnnotation(MultimediaSubtype),
    /// An XFA `script` element.
    XfaScript,
    /// An `/S` name ISO 32000-2 does not define (not kept: it is
    /// document content).
    Other,
    /// No `/S`.
    Missing,
    /// The action value is not a dictionary.
    Malformed,
}

impl ActionKind {
    /// The PDF name (`JavaScript`, `URI`, `SetOCGState`, the annotation
    /// subtype…); `-` for [`Missing`](Self::Missing), `#other`,
    /// `#malformed`, `#xfa-script`.
    pub fn as_str(self) -> &'static str {
        match self {
            ActionKind::JavaScript => "JavaScript",
            ActionKind::Launch => "Launch",
            ActionKind::Uri => "URI",
            ActionKind::GoToR => "GoToR",
            ActionKind::GoToE => "GoToE",
            ActionKind::SubmitForm => "SubmitForm",
            ActionKind::ImportData => "ImportData",
            ActionKind::ResetForm => "ResetForm",
            ActionKind::Hide => "Hide",
            ActionKind::SetOcgState => "SetOCGState",
            ActionKind::Trans => "Trans",
            ActionKind::Named => "Named",
            ActionKind::GoTo => "GoTo",
            ActionKind::GoToDp => "GoToDp",
            ActionKind::Thread => "Thread",
            ActionKind::GoTo3DView => "GoTo3DView",
            ActionKind::Rendition => "Rendition",
            ActionKind::Movie => "Movie",
            ActionKind::Sound => "Sound",
            ActionKind::RichMediaExecute => "RichMediaExecute",
            ActionKind::MultimediaAnnotation(s) => s.as_str(),
            ActionKind::XfaScript => "#xfa-script",
            ActionKind::Other => "#other",
            ActionKind::Missing => "-",
            ActionKind::Malformed => "#malformed",
        }
    }
}

/// Multimedia annotation subtypes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum MultimediaSubtype {
    RichMedia,
    Screen,
    Movie,
    Sound,
    ThreeD,
}

impl MultimediaSubtype {
    /// The `/Subtype` name.
    pub fn as_str(self) -> &'static str {
        match self {
            MultimediaSubtype::RichMedia => "RichMedia",
            MultimediaSubtype::Screen => "Screen",
            MultimediaSubtype::Movie => "Movie",
            MultimediaSubtype::Sound => "Sound",
            MultimediaSubtype::ThreeD => "3D",
        }
    }
}

/// An additional-actions trigger (`/AA` key, ISO 32000-2 Tables
/// 197–200). A key the standard does not define is
/// [`Trigger::OTHER`] (not kept: it is document content).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Trigger(pub(crate) &'static str);

impl Trigger {
    /// A key outside Tables 197–200.
    pub const OTHER: Trigger = Trigger("other");

    /// The key (`O`, `K`, `WC`, …), or `other`.
    pub fn as_str(self) -> &'static str {
        self.0
    }
}

/// Where an executable item lives. Positions, never names: pages,
/// annotations (in `/Annots`), fields (index path from `/AcroForm
/// /Fields`), outline items (index path from the first top-level item),
/// name-tree entries and XFA packets by index.
///
/// `Display` is the runner's owner notation: `catalog/OpenAction`,
/// `catalog/OpenAction#1`, `catalog/AA/WC`, `page0/AA/O`, `annot0.2`,
/// `annot0.2/A`, `annot0.2/AA/U`, `field0.1/AA/K`, `outline1.0/A`,
/// `names/JavaScript#3`, `xfa`, `xfa#2`, `xfa/across`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ActionOwner {
    /// The catalog `/OpenAction`; `member` for an array of actions.
    OpenAction {
        /// Position in the array, for an array of action dictionaries.
        member: Option<u32>,
    },
    /// A catalog `/AA` trigger.
    CatalogAa(Trigger),
    /// A page `/AA` trigger.
    PageAa {
        /// Page index.
        page: u32,
        /// The trigger.
        trigger: Trigger,
    },
    /// A multimedia annotation itself.
    Annotation {
        /// Page index.
        page: u32,
        /// Position in the page's `/Annots`.
        index: u32,
    },
    /// An annotation's `/A`.
    AnnotationA {
        /// Page index.
        page: u32,
        /// Position in the page's `/Annots`.
        index: u32,
    },
    /// An annotation `/AA` trigger.
    AnnotationAa {
        /// Page index.
        page: u32,
        /// Position in the page's `/Annots`.
        index: u32,
        /// The trigger.
        trigger: Trigger,
    },
    /// A field node's `/AA` trigger (a node that is not a page's
    /// annotation; a widget's `/AA` is [`ActionOwner::AnnotationAa`]).
    FieldAa {
        /// Index path from `/AcroForm /Fields` through `/Kids`.
        path: Vec<u32>,
        /// The trigger.
        trigger: Trigger,
    },
    /// An outline item's `/A`.
    Outline {
        /// Index path from the first top-level item.
        path: Vec<u32>,
    },
    /// A `/Names /JavaScript` entry, by position in the tree.
    NameTree {
        /// Entry index.
        index: u32,
    },
    /// An XFA packet's scripts: `packet` among the packet streams, `None`
    /// for a single XFA stream.
    Xfa {
        /// Position among the packet streams.
        packet: Option<u32>,
    },
    /// A `script` element only visible across XFA packet boundaries.
    XfaAcross,
}

impl fmt::Display for ActionOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = |p: &[u32]| {
            p.iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(".")
        };
        match self {
            ActionOwner::OpenAction { member: None } => write!(f, "catalog/OpenAction"),
            ActionOwner::OpenAction { member: Some(i) } => write!(f, "catalog/OpenAction#{i}"),
            ActionOwner::CatalogAa(t) => write!(f, "catalog/AA/{}", t.0),
            ActionOwner::PageAa { page, trigger } => write!(f, "page{page}/AA/{}", trigger.0),
            ActionOwner::Annotation { page, index } => write!(f, "annot{page}.{index}"),
            ActionOwner::AnnotationA { page, index } => write!(f, "annot{page}.{index}/A"),
            ActionOwner::AnnotationAa {
                page,
                index,
                trigger,
            } => write!(f, "annot{page}.{index}/AA/{}", trigger.0),
            ActionOwner::FieldAa { path: p, trigger } => {
                write!(f, "field{}/AA/{}", path(p), trigger.0)
            }
            ActionOwner::Outline { path: p } => write!(f, "outline{}/A", path(p)),
            ActionOwner::NameTree { index } => write!(f, "names/JavaScript#{index}"),
            ActionOwner::Xfa { packet: None } => write!(f, "xfa"),
            ActionOwner::Xfa { packet: Some(i) } => write!(f, "xfa#{i}"),
            ActionOwner::XfaAcross => write!(f, "xfa/across"),
        }
    }
}

/// The size of a script: the length of a string, or of a decoded stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ScriptSize {
    /// Bytes.
    Bytes(u64),
    /// A stream that does not decode (carried byte for byte all the
    /// same).
    Undecodable,
}

/// One carried action, multimedia annotation or XFA script.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExecutableItem {
    /// By effect.
    pub class: ActionClass,
    /// The action type, annotation subtype or XFA script.
    pub kind: ActionKind,
    /// Where it lives.
    pub owner: ActionOwner,
    /// 0 for the owner's action (or the annotation, or the first script
    /// of a packet), n for the n-th `/Next` step depth first (or the n-th
    /// script of the packet).
    pub chain_position: u32,
    /// The action's object in the input, when it is an indirect object.
    pub input_object: Option<IndirectRef>,
    /// The same object in the output.
    pub output_object: Option<IndirectRef>,
    /// For a JavaScript action: the script's size. Never the text.
    pub script: Option<ScriptSize>,
    /// The index of the first item with the same action object, when the
    /// object is shared (its chain is listed only there).
    pub shared_with: Option<usize>,
}

/// A structure the classification could not follow to the end.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UnclassifiedStructure {
    /// The owner, when the structure is below one (an action chain).
    pub owner: Option<ActionOwner>,
    /// Which structure.
    pub structure: &'static str,
    /// Why.
    pub reason: UnclassifiedReason,
}

/// Why a structure could not be classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum UnclassifiedReason {
    /// It refers back to itself.
    Cycle,
    /// Deeper than [`Limits::max_rewrite_walk_depth`].
    DepthLimit,
    /// An object could not be read.
    Unreadable,
}

/// The version the input declares (header, or catalog `/Version` when
/// later) and the one written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct VersionChange {
    /// Declared by the input.
    pub declared: (u8, u8),
    /// Written.
    pub written: (u8, u8),
    /// Why they differ; `None` when they do not.
    pub reason: Option<VersionReason>,
}

/// Why the written version is later than the declared one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VersionReason {
    /// The document carries optional content (`/OCProperties`, PDF 1.5).
    OptionalContent,
}

/// Rewrite `document` under `options`, with a report of the executable
/// content carried. With [`RewriteOptions::default()`] the bytes and
/// errors are exactly [`rewrite`](crate::rewrite)'s.
///
/// Policy refusals are [`Error::Unsupported`] (executable content,
/// multimedia) or [`Error::SignaturePolicy`]; [`Error::refusal_reason`]
/// classifies them. The detail names class, kind and owner, never
/// content.
///
/// ```
/// use kasane_pdf::{rewrite_with, Document, ExecutableContentPolicy, RewriteOptions};
/// # let bytes = include_bytes!("../tests/fixtures/hello.pdf");
/// let doc = Document::from_bytes(bytes)?;
/// let out = rewrite_with(
///     &doc,
///     &RewriteOptions::new().with_executable_content(ExecutableContentPolicy::Carry),
/// )?;
/// for item in out.report().executable_content() {
///     println!("{} {} at {}", item.class.as_str(), item.kind.as_str(), item.owner);
/// }
/// let pdf: Vec<u8> = out.into_bytes();
/// # assert!(pdf.starts_with(b"%PDF-"));
/// # Ok::<(), kasane_pdf::Error>(())
/// ```
pub fn rewrite_with(document: &Document, options: &RewriteOptions) -> Result<RewriteOutput, Error> {
    options.limits.validate()?;
    let (bytes, map, version) = crate::rewrite::rewrite_mapped(
        document,
        &options.limits,
        options.executable_content == ExecutableContentPolicy::Carry,
    )?;
    let (items, unclassified) =
        crate::executable::classify(document, &options.limits, &|r: IndirectRef| {
            map.get(&(r.number, r.generation)).map(|n| IndirectRef {
                number: *n,
                generation: 0,
            })
        });
    crate::executable::enforce(options.executable_content, &items, &unclassified)?;
    Ok(RewriteOutput {
        bytes,
        report: RewriteReport {
            executable_content: options.executable_content,
            multimedia: options.multimedia,
            usage_rights: options.usage_rights,
            items,
            unclassified,
            version,
        },
    })
}
