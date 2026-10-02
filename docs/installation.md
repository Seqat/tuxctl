# Installation

## Requirements

- **Linux.** `tuxctl` uses Linux interfaces such as `/proc` and `/sys`
  directly.
- `systemctl` for the Services screen and `journalctl` for the Logs screen.
- **Rust 1.88 or newer** to build from source.

Process signals use Linux pidfds so that a signal can never reach a process
that reused the PID. If pidfds are unavailable, `tuxctl` refuses to send the
signal rather than fall back to PID-only signaling.

## Prebuilt binaries

Each [release](https://github.com/Seqat/tuxctl/releases/latest) has statically
linked (musl) binaries for `x86_64` and `aarch64`. They do not depend on the
system C library, so they run on any Linux distribution. Download one, verify
it, and install it:

```sh
arch=$(uname -m)   # x86_64 or aarch64
base=https://github.com/Seqat/tuxctl/releases/latest/download
curl -LO "$base/tuxctl-$arch-unknown-linux-musl.tar.gz" -LO "$base/SHA256SUMS"
sha256sum -c --ignore-missing SHA256SUMS
tar xzf "tuxctl-$arch-unknown-linux-musl.tar.gz"
install -Dm755 "tuxctl-$arch-unknown-linux-musl/tuxctl" ~/.local/bin/tuxctl
```

`~/.local/bin` must be on your `PATH`.

From v0.3.4 on, each archive also has a build provenance attestation. With the
GitHub CLI, this confirms that the archive was built by the project's release
workflow from its repository:

```sh
gh attestation verify "tuxctl-$arch-unknown-linux-musl.tar.gz" --repo Seqat/tuxctl
```

### NVIDIA GPUs: the glibc binary

The static binaries cannot load NVIDIA's NVML library, so they show no
temperature or usage for NVIDIA GPUs on the proprietary driver. From v0.3.4
on, each release also has an `x86_64` binary linked against glibc, which can.
It needs glibc 2.28 or newer (RHEL 8, Debian 10, Ubuntu 20.04, and later
releases of most other distributions); `ldd --version` shows yours.

```sh
base=https://github.com/Seqat/tuxctl/releases/latest/download
curl -LO "$base/tuxctl-x86_64-unknown-linux-gnu.tar.gz" -LO "$base/SHA256SUMS"
sha256sum -c --ignore-missing SHA256SUMS
tar xzf tuxctl-x86_64-unknown-linux-gnu.tar.gz
install -Dm755 tuxctl-x86_64-unknown-linux-gnu/tuxctl ~/.local/bin/tuxctl
gh attestation verify tuxctl-x86_64-unknown-linux-gnu.tar.gz --repo Seqat/tuxctl
```

On `aarch64`, or with an older glibc, build from source instead (see
[NVIDIA GPUs](optional-setup.md#nvidia-gpus)).

## From crates.io

```sh
cargo install tuxctl --locked
```

This builds against the system's glibc, so it can load NVML and show NVIDIA
GPUs on the proprietary driver.

## From the repository

A tagged version straight from GitHub:

```sh
cargo install --git https://github.com/Seqat/tuxctl --tag v0.3.3 --locked
```

Or from a clone:

```sh
git clone https://github.com/Seqat/tuxctl.git
cd tuxctl
cargo install --path . --locked
```

To build an optimized binary without installing it:

```sh
cargo build --release --locked
./target/release/tuxctl
```

## First run

```sh
tuxctl            # start on the Overview
tuxctl --check    # list the sensors tuxctl finds, then exit
```

Everything works without setup. A few sensors need a step on some machines;
`tuxctl --check` says which, and [Optional setup](optional-setup.md) explains
each one.
