//! Command-line parsing (standard library only).

use std::time::Duration;

use crate::{
    about,
    app::{DEFAULT_SAMPLING_INTERVAL, SAMPLING_PRESETS},
};

pub const USAGE: &str =
    "Usage: tuxctl [--interval <DURATION>] [--no-nvidia-temperature]  (see --help)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Run {
        interval: Duration,
        /// Read NVIDIA GPU temperatures through NVML, where it can be loaded
        /// (on by default; it costs about 20 MiB of private memory once an
        /// NVIDIA GPU is read).
        nvidia_temperature: bool,
    },
    Help,
    Version,
}

/// Parses the arguments after the program name.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut interval = DEFAULT_SAMPLING_INTERVAL;
    let mut nvidia_temperature = NVML_AVAILABLE;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "--interval" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--interval requires a value".to_owned())?;
                interval = parse_interval(&value)?;
            }
            "--no-nvidia-temperature" => nvidia_temperature = false,
            _ => match arg.strip_prefix("--interval=") {
                Some(value) => interval = parse_interval(value)?,
                None => return Err(format!("unexpected argument '{arg}'")),
            },
        }
    }
    Ok(Command::Run {
        interval,
        nvidia_temperature,
    })
}

/// Static musl builds cannot load the (glibc) NVIDIA library.
pub const NVML_AVAILABLE: bool = !cfg!(target_env = "musl");

fn parse_interval(value: &str) -> Result<Duration, String> {
    SAMPLING_PRESETS
        .iter()
        .find(|(name, _)| *name == value)
        .map(|(_, interval)| *interval)
        .ok_or_else(|| {
            format!(
                "invalid interval '{value}' (expected one of: {})",
                preset_names()
            )
        })
}

fn preset_names() -> String {
    SAMPLING_PRESETS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn version_text() -> String {
    format!("{} {}", about::NAME, about::VERSION)
}

pub fn help_text() -> String {
    format!(
        "{name} {version} - {description}

Usage: {name} [OPTIONS]

Options:
      --interval <DURATION>  Sampling interval for CPU, memory and network (default: 1s)
                             One of: {presets}
                             Processes refresh at most once per second, services every 5s.
                             Press + / - inside {name} to change it while running.
      --no-nvidia-temperature
                             Do not load NVML for NVIDIA GPUs on the proprietary driver.
                             It shows their temperature and adds about 20 MiB of private
                             memory. Static (musl) builds never load it.
  -h, --help                 Print help
  -V, --version              Print version

Press ? inside {name} for key bindings.
License: {license}   Source: {repository}
",
        name = about::NAME,
        version = about::VERSION,
        description = about::DESCRIPTION,
        presets = preset_names(),
        license = about::LICENSE,
        repository = about::REPOSITORY,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn no_arguments_run_with_the_default_interval() {
        assert_eq!(
            parse_args(&[]),
            Ok(Command::Run {
                interval: Duration::from_secs(1),
                nvidia_temperature: NVML_AVAILABLE,
            })
        );
    }

    #[test]
    fn every_preset_is_accepted_in_both_forms() {
        for (name, interval) in SAMPLING_PRESETS {
            let expected = Ok(Command::Run {
                interval,
                nvidia_temperature: NVML_AVAILABLE,
            });
            assert_eq!(parse_args(&["--interval", name]), expected, "{name}");
            assert_eq!(
                parse_args(&[&format!("--interval={name}")]),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn invalid_intervals_are_rejected_with_the_accepted_values() {
        for value in ["100ms", "1", "1.5s", "0s", "2m", "", "-1s"] {
            let error = parse_args(&["--interval", value]).unwrap_err();
            assert!(
                error.contains("250ms 500ms 1s 2s 5s 10s 30s 60s"),
                "{value}: {error}"
            );
        }
        assert_eq!(
            parse_args(&["--interval"]),
            Err("--interval requires a value".to_owned())
        );
    }

    #[test]
    fn nvidia_temperature_is_on_where_nvml_can_load_and_can_be_turned_off() {
        assert_eq!(
            parse_args(&["--no-nvidia-temperature", "--interval", "2s"]),
            Ok(Command::Run {
                interval: Duration::from_secs(2),
                nvidia_temperature: false,
            })
        );
        assert_eq!(NVML_AVAILABLE, !cfg!(target_env = "musl"));
        for arg in ["--nvidia-temperature", "--no-nvidia-temperature=1"] {
            assert!(parse_args(&[arg]).is_err(), "{arg}");
        }
    }

    #[test]
    fn unknown_arguments_are_errors() {
        for arg in ["--intervals=1s", "-i", "run", "--verbose"] {
            assert!(parse_args(&[arg]).is_err(), "{arg}");
        }
    }

    #[test]
    fn help_and_version_win_over_other_arguments() {
        assert_eq!(parse_args(&["--interval", "5s", "-h"]), Ok(Command::Help));
        assert_eq!(parse_args(&["--help", "--bogus"]), Ok(Command::Help));
        assert_eq!(parse_args(&["-V"]), Ok(Command::Version));
        assert_eq!(parse_args(&["--version"]), Ok(Command::Version));
    }

    #[test]
    fn version_and_help_come_from_the_package_metadata() {
        assert_eq!(
            version_text(),
            format!("tuxctl {}", env!("CARGO_PKG_VERSION"))
        );
        let help = help_text();
        assert!(help.contains("--interval <DURATION>"));
        assert!(help.contains("--no-nvidia-temperature"));
        assert!(USAGE.contains("--no-nvidia-temperature"));
        assert!(help.contains(env!("CARGO_PKG_REPOSITORY")));
    }
}
