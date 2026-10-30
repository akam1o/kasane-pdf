//! `--password-file`: encrypted documents from the command line.

mod common;

use common::{bin, fixture, tmpdir};
use std::fs;
use std::io::Write;
use std::process::Stdio;

fn enc(name: &str) -> String {
    fixture(&format!("encrypted/{name}"))
        .to_string_lossy()
        .into_owned()
}

#[test]
fn password_file_opens_and_wrong_password_is_refused() {
    let dir = tmpdir("password");
    let good = dir.join("good.txt");
    let bad = dir.join("bad.txt");
    fs::write(&good, "usr\n").unwrap();
    fs::write(&bad, "hunter2\n").unwrap();

    // No password: the user password is not empty.
    let out = bin().arg(enc("enc_r6_aes256.pdf")).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("[encrypted document]"));

    let out = bin()
        .args(["--password-file", good.to_str().unwrap()])
        .arg(enc("enc_r6_aes256.pdf"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("encryption: Standard R6"), "{stdout}");
    assert!(stdout.contains("opened as User"), "{stdout}");

    // The option may follow the subcommand's arguments too.
    let out = bin()
        .args(["text", &enc("enc_r3_rc4_128.pdf"), "1", "--format", "lines"])
        .args(["--password-file", good.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"H\""));

    // A wrong password is refused, and never echoed.
    let out = bin()
        .args(["--password-file", bad.to_str().unwrap()])
        .arg(enc("enc_r6_aes256.pdf"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(all.contains("[encrypted document]") && !all.contains("hunter2"));
}

#[test]
fn password_from_stdin_with_crlf_and_as_owner() {
    let mut child = bin()
        .args(["--password-file", "-", &enc("enc_r4_aes128.pdf")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"own\r\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("opened as Owner"));
}

#[test]
fn missing_password_file_argument_is_a_usage_error() {
    let out = bin().arg("--password-file").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let out = bin()
        .args([
            "--password-file",
            "/nonexistent/kasane-pw",
            &enc("base.pdf"),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn writers_refuse_an_encrypted_input() {
    let dir = tmpdir("password-rewrite");
    let out = bin()
        .args(["rewrite", &enc("enc_r6_aes256_empty.pdf"), "--output"])
        .arg(dir.join("out.pdf"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("writing an encrypted document"));
    assert!(!dir.join("out.pdf").exists());
}
