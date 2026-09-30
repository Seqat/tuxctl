# Security Policy

## Supported versions

Only the latest release receives security fixes. `tuxctl` is maintained by one
person in their spare time, so fixes land in a new release rather than being
backported.

| Version | Supported |
| --- | --- |
| latest 0.3.x | yes |
| older | no |

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub:
**[Report a vulnerability](https://github.com/Seqat/tuxctl/security/advisories/new)**
(the repository's Security tab → *Report a vulnerability*). Do not open a public
issue for a security problem.

Include the `tuxctl` version (`tuxctl --version`), how you installed it, your
distribution and kernel, and the steps or input that trigger the problem.

What to expect:

- an acknowledgement within about a week;
- a fix in a new release when the report is confirmed, with a GitHub security
  advisory crediting you unless you prefer otherwise;
- coordinated disclosure: please keep details private until the fixed release
  is out.

## Scope

`tuxctl` shows data that other local users control (process names and command
lines, journal messages, unit descriptions, interface names) and can send
signals to processes, sometimes while running as root. Examples of issues that
are in scope:

- text from system data that reaches the terminal as escape or control
  sequences;
- a signal reaching a different process than the one confirmed (for example
  through PID reuse);
- anything that lets another local user make `tuxctl` run commands, load a
  library they control, or read or write files on its behalf;
- crashes or hangs that another local user can trigger.

Hardware data shown wrongly, or root-only sensors that stay hidden, are
ordinary bugs; please use the issue tracker for those.

## Verifying releases

- Release archives are listed in `SHA256SUMS` on each GitHub release.
- From v0.3.4 on, each archive also has a build provenance attestation:
  `gh attestation verify tuxctl-<arch>-unknown-linux-musl.tar.gz --repo Seqat/tuxctl`
  confirms it was built by this repository's release workflow.
- The crates.io package is published from the release workflow through
  crates.io Trusted Publishing; no long-lived publish token exists.
