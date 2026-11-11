//! `magnolia render`, and the misuses every command shares: the file it writes is the library's
//! render to the byte, whatever the block; it never writes over a file, and leaves none behind
//! when it fails.

use std::ffi::OsString;
use std::fs;
use std::io::Cursor;
use std::os::unix::ffi::OsStringExt;

use magnolia_session::generate::{Kind, generate};
use magnolia_session::{Session, render};

use crate::support::{TempDir, magnolia};

#[test]
fn render_writes_the_librarys_render_whatever_the_block() {
    let dir = TempDir::new("render");
    generate(Kind::Small, dir.path()).unwrap();
    let session_path = dir.path().join("session.json");
    let session = Session::open(&session_path).unwrap();
    let mut expected = Cursor::new(Vec::new());
    render(&session, 4_096, &mut expected).unwrap();
    let expected = expected.into_inner();

    let out = dir.path().join("render.wav");
    let ran = magnolia([
        OsString::from("render"),
        session_path.clone().into(),
        out.clone().into(),
    ]);
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    assert!(ran.stdout.starts_with("Rendered "), "{}", ran.stdout);
    assert!(ran.stdout.contains("0 underruns"), "{}", ran.stdout);
    assert!(
        fs::read(&out).unwrap() == expected,
        "the file differs from the library's render"
    );

    let out_64 = dir.path().join("render-64.wav");
    let ran = magnolia([
        OsString::from("render"),
        session_path.clone().into(),
        out_64.clone().into(),
        "--block".into(),
        "64".into(),
    ]);
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    assert!(
        fs::read(&out_64).unwrap() == expected,
        "the file rendered in blocks of 64 differs from the library's render"
    );
}

#[test]
fn render_never_writes_over_a_file() {
    let dir = TempDir::new("render-over");
    generate(Kind::Small, dir.path()).unwrap();
    let out = dir.path().join("keep.wav");
    fs::write(&out, "the user's").unwrap();
    let ran = magnolia([
        OsString::from("render"),
        dir.path().join("session.json").into(),
        out.clone().into(),
    ]);
    assert_eq!(ran.code, Some(1));
    assert!(
        ran.stderr.contains("keep.wav cannot be created"),
        "{}",
        ran.stderr
    );
    assert_eq!(fs::read_to_string(&out).unwrap(), "the user's");
}

#[test]
fn a_failed_render_leaves_no_file() {
    let dir = TempDir::new("render-block");
    generate(Kind::Small, dir.path()).unwrap();
    let out = dir.path().join("render.wav");
    for block in ["0", "240001"] {
        let ran = magnolia([
            OsString::from("render"),
            dir.path().join("session.json").into(),
            out.clone().into(),
            "--block".into(),
            block.into(),
        ]);
        assert_eq!(ran.code, Some(1), "--block {block}");
        assert!(
            ran.stderr
                .contains("a render's block holds 1 to 240000 frames"),
            "{}",
            ran.stderr
        );
        assert!(!out.exists(), "--block {block} left a file");
    }
}

#[test]
fn a_bad_session_is_a_plain_error() {
    let dir = TempDir::new("render-bad");
    let out = dir.path().join("render.wav");
    let broken = dir.path().join("broken.json");
    fs::write(&broken, "{ \"format\": ").unwrap();
    let missing = dir.path().join("missing.json");
    for (session, expected) in [
        (&broken, "broken.json"),
        (&missing, "missing.json cannot be read"),
    ] {
        for command in ["render", "play"] {
            let mut arguments = vec![OsString::from(command), session.clone().into()];
            if command == "render" {
                arguments.push(out.clone().into());
            }
            let ran = magnolia(arguments);
            assert_eq!(ran.code, Some(1), "{command}: {}", ran.stderr);
            assert!(ran.stderr.starts_with("magnolia: "), "{}", ran.stderr);
            assert!(ran.stderr.contains(expected), "{}", ran.stderr);
            assert!(!ran.stderr.contains("Usage"), "a failure is not a misuse");
        }
    }
    assert!(!out.exists(), "a session that does not open leaves no file");
}

#[test]
fn a_misuse_exits_with_2_and_the_usage() {
    let not_text = OsString::from_vec(vec![b'-', 0xFF]);
    let misuses: [Vec<OsString>; 5] = [
        vec![],
        vec!["paly".into(), "session.json".into()],
        vec!["render".into(), "session.json".into()],
        vec![
            "play".into(),
            "session.json".into(),
            "--seconds".into(),
            "soon".into(),
        ],
        vec!["play".into(), "session.json".into(), not_text],
    ];
    for arguments in misuses {
        let ran = magnolia(&arguments);
        assert_eq!(ran.code, Some(2), "{arguments:?}: {}", ran.stderr);
        assert!(ran.stderr.starts_with("magnolia: "), "{}", ran.stderr);
        assert!(ran.stderr.contains("Usage:"), "{}", ran.stderr);
        assert!(ran.stdout.is_empty());
    }
}

#[test]
fn help_prints_the_usage() {
    for flag in ["--help", "-h", "help"] {
        let ran = magnolia([flag]);
        assert_eq!(ran.code, Some(0));
        assert!(ran.stdout.starts_with("Usage:"), "{}", ran.stdout);
        assert!(ran.stdout.contains("magnolia play <session.json>"));
        assert!(
            ran.stdout
                .contains("magnolia render <session.json> <out.wav>")
        );
        assert!(ran.stderr.is_empty());
    }
}
