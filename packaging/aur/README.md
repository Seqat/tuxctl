# AUR packages

Two packages are maintained from these files:

| Package | Builds from | NVIDIA (NVML) |
| --- | --- | --- |
| [`tuxctl`](tuxctl/PKGBUILD) | the crates.io source, against glibc | yes, with `nvidia-utils` |
| [`tuxctl-bin`](tuxctl-bin/PKGBUILD) | the static musl release archives | no |

Each directory here mirrors one AUR Git repository
(`ssh://aur@aur.archlinux.org/<package>.git`), which also needs a
`.SRCINFO` generated from the PKGBUILD.

## Updating after a release

Run on Arch Linux, after the GitHub release and the crates.io publish have
finished (see `RELEASING.md`):

```sh
cd packaging/aur/tuxctl          # then the same in tuxctl-bin
sed -i "s/^pkgver=.*/pkgver=X.Y.Z/; s/^pkgrel=.*/pkgrel=1/" PKGBUILD
updpkgsums                       # from pacman-contrib
makepkg -Ccsi                    # build in a clean tree, run check(), install
namcap PKGBUILD *.pkg.tar.zst
makepkg --printsrcinfo > .SRCINFO
```

For `tuxctl-bin`, check that the sums `updpkgsums` writes match `SHA256SUMS`
on the release, and optionally
`gh attestation verify <archive> --repo Seqat/tuxctl`.

Then copy `PKGBUILD` and `.SRCINFO` into the AUR clone, commit
(`Update to X.Y.Z`) and push. Commit the updated PKGBUILD here too.

## Notes

- Do not add `--all-features` to `cargo build`: `redraw-counter` is a
  development-only feature for `scripts/measure.py`.
- `options=('!debug')`: the release profile strips the binary, so a debug
  package would be empty.
- `check()` runs the test suite, which reads `/proc` and spawns `sh`; it works
  in a clean chroot (`extra-x86_64-build`).
