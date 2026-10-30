//! Open a possibly encrypted document: without a password first (the
//! empty user password, as every reader tries), then with a password
//! read from a file, and report what the `/Encrypt` dictionary
//! declares. Encrypted documents are read only — the writers refuse
//! them.
//!
//! ```console
//! cargo run --example open_encrypted -- tests/fixtures/encrypted/enc_r6_aes256_empty.pdf
//! printf 'usr\n' > /tmp/pw && \
//!   cargo run --example open_encrypted -- tests/fixtures/encrypted/enc_r6_aes256.pdf /tmp/pw
//! ```

use kasane_pdf::{Document, Error, OpenOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: open_encrypted <input.pdf> [password-file]")?;
    let data = std::fs::read(path)?;

    let doc = match Document::from_bytes(&data) {
        Ok(doc) => doc,
        // The empty user password does not open it: ask for one. The
        // password is bytes, normally UTF-8 text; it is tried as user
        // and then as owner password, and never appears in errors.
        Err(Error::Encrypted) => {
            let pw_file = args.next().ok_or("encrypted: pass a password file")?;
            let pw = std::fs::read_to_string(pw_file)?;
            let pw = pw.lines().next().unwrap_or("");
            match Document::open(&data, &OpenOptions::new().with_password(pw)) {
                Ok(doc) => doc,
                Err(Error::Encrypted) => return Err("wrong password".into()),
                Err(e) => return Err(e.into()),
            }
        }
        // A malformed /Encrypt dictionary is Error::Structure; a
        // security handler or method the engine does not implement is
        // Error::Unsupported. No password helps with either.
        Err(e) => return Err(e.into()),
    };

    match doc.encryption() {
        None => println!("not encrypted"),
        Some(e) => {
            println!(
                "Standard security handler R{} (V{}), {}-bit key; strings {:?}, streams {:?}",
                e.revision, e.version, e.key_bits, e.string_method, e.stream_method
            );
            println!("opened as {:?}", e.authenticated_as);
            if e.empty_user_password {
                // Anyone can open such a file: the encryption gives no
                // confidentiality, whatever the cipher.
                println!("empty user password: no confidentiality");
            }
            // Permissions are the author's request, not access control:
            // whoever can open the document can read all of it.
            println!(
                "permissions (advisory): print {}, modify {}, copy {}, annotate {}",
                e.permission(3),
                e.permission(4),
                e.permission(5),
                e.permission(6)
            );
        }
    }
    println!("{} page(s)", doc.page_count());

    // Writing an encrypted document is refused (it would drop or break
    // the encryption).
    if let Err(e) = kasane_pdf::rewrite(&doc) {
        println!("rewrite: {e}");
    }
    for w in doc.take_warnings() {
        println!("warning: {w}");
    }
    Ok(())
}
