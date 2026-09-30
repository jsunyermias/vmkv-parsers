#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

fn fake_parsers() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-parse-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, body: &str, mode: u32| {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    write("vmkv-parser-fake", "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'vmkv-parser-fake 9.9.9'; exit 0; fi\necho \"args:$*\"\necho err >&2\nexit 7\n", 0o755);
    write("vmkv-parser-noexec", "#!/bin/sh\nexit 0\n", 0o644);
    write("not-a-parser", "#!/bin/sh\nexit 0\n", 0o755);
    dir
}

fn launcher(dir: &PathBuf, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_vmkv-parse")).args(args).env("PATH", dir).output().unwrap()
}

#[test]
fn passes_through_args_stdout_stderr_and_exit_code() {
    let dir = fake_parsers();
    let o = launcher(&dir, &["fake", "-o", "x.vtj", "in put.mp3"]);
    assert_eq!(o.status.code(), Some(7));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "args:-o x.vtj in put.mp3\n");
    assert_eq!(String::from_utf8_lossy(&o.stderr), "err\n");
}

#[test]
fn unknown_or_invalid_codec_is_a_usage_error() {
    let dir = fake_parsers();
    for args in [&["mp4"][..], &["noexec"], &["../fake"], &["Fake"], &[]] {
        let o = launcher(&dir, args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert!(o.stdout.is_empty(), "{args:?}");
    }
    let o = launcher(&dir, &["mp4"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("found:"), "lists known codecs");
}

#[test]
fn lists_parsers_with_versions() {
    let dir = fake_parsers();
    let o = launcher(&dir, &["--list"]);
    let out = String::from_utf8_lossy(&o.stdout);
    let fake = out.lines().find(|l| l.starts_with("fake\t")).expect("fake listed");
    assert!(fake.contains("vmkv-parser-fake 9.9.9"), "{fake}");
    assert!(!out.contains("noexec"), "non-executables are ignored: {out}");
    assert!(!out.contains("not-a-parser"), "{out}");
    assert_eq!(launcher(&dir, &["--help"]).status.code(), Some(0));
}
