//! Command-line behavior of the real binary. Arguments are handled before the
//! terminal is touched, so none of these runs may write an escape sequence.

use std::process::{Command, Output, Stdio};

fn run(args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_tuxctl"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run tuxctl");
    for (name, stream) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
        assert!(
            !stream.contains(&0x1b),
            "{args:?} wrote an escape sequence to {name}"
        );
    }
    output
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).expect("UTF-8 output")
}

#[test]
fn version_prints_the_package_version() {
    for flag in ["--version", "-V"] {
        let output = run(&[flag]);
        assert_eq!(output.status.code(), Some(0), "{flag}");
        assert_eq!(
            text(&output.stdout),
            format!("tuxctl {}\n", env!("CARGO_PKG_VERSION"))
        );
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn help_describes_the_options() {
    for flag in ["--help", "-h"] {
        let output = run(&[flag]);
        assert_eq!(output.status.code(), Some(0), "{flag}");
        let help = text(&output.stdout);
        assert!(help.contains("--interval <DURATION>"), "{help}");
        assert!(help.contains("--version"), "{help}");
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn invalid_arguments_exit_with_usage() {
    for args in [
        &["--bogus"][..],
        &["--interval", "3s"],
        &["--interval"],
        &["--interval=100ms"],
    ] {
        let output = run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        let error = text(&output.stderr);
        assert!(error.starts_with("tuxctl: "), "{error}");
        assert!(error.contains("Usage: tuxctl"), "{error}");
    }
}

#[test]
fn a_valid_interval_does_not_prevent_version() {
    let output = run(&["--interval=250ms", "--version"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(text(&output.stdout).starts_with("tuxctl "));
}
