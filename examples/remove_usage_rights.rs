//! Remove usage-rights signatures (`/Perms /UR`, `/UR3`) with
//! `UsageRightsPolicy::Remove`, and show what the report says was
//! removed. Any other signature, a field lock, `/DSS`, `/Legal` or a
//! document that cannot be searched completely is refused.
//!
//! ```console
//! cargo run --example remove_usage_rights -- input.pdf out.pdf [--carry]
//! ```
//!
//! `--carry` also carries executable content (`ExecutableContentPolicy::Carry`):
//! most usage-rights forms have a document-level script, which the default
//! policy refuses. The output prints counts and kinds only, never
//! signature values.

use kasane_pdf::{
    rewrite_with, Document, Error, ExecutableContentPolicy, RewriteOptions, UsageRightsOutcome,
    UsageRightsPolicy,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: remove_usage_rights <input.pdf> <out.pdf> [--carry]");
        std::process::exit(2);
    }
    let doc = Document::from_bytes(&std::fs::read(&args[1])?)?;
    let mut opts = RewriteOptions::new().with_usage_rights(UsageRightsPolicy::Remove);
    if args.iter().any(|a| a == "--carry") {
        opts = opts.with_executable_content(ExecutableContentPolicy::Carry);
    }
    match rewrite_with(&doc, &opts) {
        Ok(out) => {
            match out.report().usage_rights() {
                UsageRightsOutcome::Removed(r) => {
                    println!(
                        "removed {} usage-rights signature(s); /Perms dropped: {}",
                        r.entries.len(),
                        r.perms_removed
                    );
                    for e in &r.entries {
                        println!("  {:?}: rights {:?}", e.key, e.rights);
                    }
                    if let Some(f) = r.sig_flags {
                        println!("  /SigFlags {} -> {}", f.before, f.after);
                    }
                    let c = r.output_check;
                    println!(
                        "  output checked: {} objects, {} pages, {} removed",
                        c.objects, c.pages, c.objects_removed
                    );
                }
                UsageRightsOutcome::NonePresent => println!("no usage-rights signature"),
                other => println!("{other:?}"),
            }
            std::fs::write(&args[2], out.bytes())?;
        }
        Err(e @ Error::SignaturePolicy { .. }) => {
            eprintln!("refused: {e}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("refused or failed [{}]: {e}", e.kind());
            std::process::exit(1);
        }
    }
    Ok(())
}
