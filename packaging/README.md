# Packaging

Definitions for the three families, plus what it actually takes to get each one
into a repository people can install from.

| Family | Files |
|---|---|
| Arch | [`arch/PKGBUILD`](arch/PKGBUILD) |
| Fedora / RHEL | [`rpm/hyprdmc.spec`](rpm/hyprdmc.spec) |
| Debian / Ubuntu | [`debian/`](debian/) |
| any | [`systemd/hyprdmc.service`](systemd/hyprdmc.service) |

All three install the same two things: `/usr/bin/hyprdmc`, and the systemd
**user** unit in `/usr/lib/systemd/user/`. Nothing runs as root — the daemon
drives one login session's displays and talks to that session's socket.

## Build them, from any distribution

```sh
./packaging/build-in-container.sh          # both, into dist/
./packaging/build-in-container.sh deb      # just one
```

The `.deb` and the `.rpm` are built **inside containers of the target
distributions**, and that is not a convenience: a `.deb` built on an Arch
machine links against Arch's glibc and breaks on Debian. Containers also mean
nothing gets installed on the machine you run this from — no `rpmbuild`, no
`debhelper`, no `sudo`. Either podman or docker will do.

The Arch package is built natively, since Arch is where `makepkg` belongs:

```sh
cd packaging/arch && updpkgsums && makepkg -si
```

## Publishing to the AUR

```sh
./packaging/arch/publish-aur.sh --dry-run   # shows exactly what would be pushed
./packaging/arch/publish-aur.sh
```

The script builds the AUR repository in a temporary directory from the `PKGBUILD`
kept here, so there is no second copy to drift. An AUR repository holds the recipe
and nothing else — `PKGBUILD` and `.SRCINFO`, never a tarball, never a built
package. `.SRCINFO` is what the AUR actually reads: it never executes the
`PKGBUILD`, so a `.SRCINFO` that is missing or stale is a package whose metadata is
wrong on the site.

Two things the script checks before touching anything, because both are outside
what it can fix:

1. **The tag must be pushed to GitHub.** `source=` points at
   `archive/refs/tags/v$pkgver.tar.gz`, and the checksum is computed from what that
   downloads. Without the tag the AUR would fetch a 404.
2. **Your AUR SSH key must be registered on your AUR account**
   (<https://aur.archlinux.org/account> → *SSH Public Key*). Verify with
   `ssh aur@aur.archlinux.org help`.

The AUR only accepts pushes to `master`, whatever the local default branch is
called; the script renames accordingly. The repository is created by the first
push — cloning a package that does not exist yet warns and gives you an empty
directory, which is expected.

Afterwards, copy the checksum the script prints back into `arch/PKGBUILD`, so the
committed recipe stops saying `SKIP`.

## Minimum Rust version, and what it costs Debian

The crate needs **rustc 1.88**: two `let` chains in `src/daemon.rs` and
`src/monitor.rs`, and one in the `ignore` crate, which is a dependency. That
number is not decorative — `Cargo.toml` declared 1.87 until a build against
exactly 1.87 proved otherwise.

The consequence is entirely on Debian's side:

| | rustc | builds |
|---|---|---|
| Debian 13 stable (trixie) | 1.85 | **no** |
| Debian unstable (sid) | 1.95 | yes |
| Fedora 42 | 1.95 | yes |
| Arch | current | yes |

This is less of a problem than it looks: new packages enter Debian through
*unstable*, never through stable, so unstable is the correct target anyway.
Stable users get it through backports, or through the vendored build in
`build-in-container.sh`.

## Two traps, both already sprung here

**`dh_clean` deletes every `*.orig` in the tree** — including the
`Cargo.toml.orig` that `cargo vendor` writes into each vendored crate. The
build then fails with `failed to calculate checksum of:
vendor/anyhow/Cargo.toml.orig`. `debian/rules` overrides it with
`dh_clean -X.orig`.

**`%autorelease` and `%autochangelog` only resolve inside `fedpkg`.** They are
what Fedora's own packages use, but a plain `rpmbuild -ba` on a clone — which
is what COPR does, and what anyone reading this repository will do — fails on
them. The spec carries an explicit `Release:` and `%changelog` instead.

## What the linters say

`build-in-container.sh` runs `lintian` and `rpmlint` on what it produced. What
is left is expected, and listed here so nobody has to wonder whether it was
looked at.

| Tool | Message | Why it stays |
|---|---|---|
| both | no manual page | Real gap. Debian policy says every binary should have one; generating it means adding `clap_mangen` as a build dependency and a `build.rs`. |
| rpmlint | `spelling-error … hotplug` | rpmlint's dictionary does not know the word. It is the correct term and the one the rest of the documentation uses. |
| lintian | `initial-upload-closes-no-bugs` | Only meaningful inside the Debian archive, where a first upload closes the ITP bug. |
| lintian | `debug-file-with-no-debug-symbols` | The `-dbgsym` package `dh` builds automatically from a release binary that carries none. |

Two findings were real and are fixed: a changelog dated Saturday for a day that
is a Sunday, and `%cargo_install` copying the crate sources into
`%{cargo_registry}` — this crate has a lib target beside its binary, and the
unpackaged files were a hard `rpmbuild` error.

## What was actually built

Not "should build". These came out of the containers:

```
hyprdmc-0.1.0-1.fc42.x86_64.rpm        1.6 MB
hyprdmc_0.1.0-1_amd64.deb              1.5 MB
hyprdmc-0.1.0-1-x86_64.pkg.tar.zst     2.0 MB
```

Each contains `/usr/bin/hyprdmc`, the systemd user unit, the licence and the
README — and nothing else. The RPM and the Arch package run the full test suite
during the build, so a broken tree cannot be packaged.

## Before any of it: tag a release

Every `Source` line points at `v$version` on GitHub. Nothing below works until
that tag exists:

```sh
git tag -a v0.1.0 -m "hyprdmc 0.1.0"
git push origin v0.1.0
gh release create v0.1.0 --generate-notes
```

Keep `pkgver` / `Version:` / `debian/changelog` in step with `Cargo.toml`.

## Build them locally

```sh
# Arch
cd packaging/arch && updpkgsums && makepkg -si

# Fedora / RHEL
rpmbuild -ba packaging/rpm/hyprdmc.spec

# Debian / Ubuntu — debian/ has to sit at the root of the source tree
cp -r packaging/debian . && dpkg-buildpackage -us -uc -b
```

## Arch → the AUR

The most direct of the three: the AUR is a git repository you push to, with no
review queue and no sponsor.

1. Create an account on <https://aur.archlinux.org> and add your SSH public key
   to it.
2. Clone the (empty) package repository — the name is the package name:

   ```sh
   git clone ssh://aur@aur.archlinux.org/hyprdmc.git aur-hyprdmc
   cd aur-hyprdmc
   ```

3. Copy the `PKGBUILD` in, fill in the real checksums, and generate the
   metadata file the AUR indexes:

   ```sh
   cp ../packaging/arch/PKGBUILD .
   updpkgsums                        # replaces sha256sums=('SKIP')
   makepkg --printsrcinfo > .SRCINFO # required, and must match the PKGBUILD
   ```

4. Check it the way the maintainers will:

   ```sh
   makepkg -f          # it must build from a clean checkout
   namcap PKGBUILD *.pkg.tar.zst
   ```

5. Push. Only `PKGBUILD`, `.SRCINFO` and any small patches belong in that
   repository — never the built package, never the sources:

   ```sh
   git add PKGBUILD .SRCINFO
   git commit -m "Initial import: hyprdmc 0.1.0"
   git push
   ```

Updating later is the same loop: bump `pkgver`, reset `pkgrel=1`,
`updpkgsums`, regenerate `.SRCINFO`, push. A `hyprdmc-git` variant building
from `main` is a separate AUR package by convention.

## Fedora / RHEL → COPR, then maybe the official repositories

**COPR** is the pragmatic route: a build service anyone can use, no review, and
users enable it with one command.

```sh
dnf install copr-cli
copr-cli create hyprdmc --chroot fedora-42-x86_64 --chroot fedora-41-x86_64
copr-cli build hyprdmc path/to/hyprdmc-0.1.0-1.src.rpm
```

Users then:

```sh
dnf copr enable jacquesh82/hyprdmc
dnf install hyprdmc
```

**The official Fedora repositories** are a different commitment. A new package
needs a Package Review bug on Bugzilla, and a first-time packager needs a
sponsor from the packager group. For Rust specifically, Fedora's guidelines
expect `rust2rpm` and one RPM per crate; an application may instead vendor its
dependencies, but then every bundled crate has to be declared:

```spec
Provides: bundled(crate(anyhow)) = 1.0.100
```

Generate that list rather than writing it by hand — `cargo tree` or
`cargo-vendor-filterer` will give it to you. Expect weeks, not hours.

## Debian / Ubuntu → your own repository, a PPA, or the archive

**Your own APT repository** is the realistic first step. Build the `.deb`,
then serve it with [aptly](https://www.aptly.info/) or `reprepro`:

```sh
aptly repo create -distribution=stable -component=main hyprdmc
aptly repo add hyprdmc ../hyprdmc_0.1.0-1_amd64.deb
aptly publish repo hyprdmc
```

Users add it with a `deb [signed-by=…]` line and your signing key.

**Ubuntu PPA** (Launchpad) is the same idea with the hosting done for you:
create an account and a PPA, sign the source package with your GPG key, then
`dput ppa:jacquesh82/hyprdmc ../hyprdmc_0.1.0-1_source.changes`. Launchpad
builds it for each Ubuntu series. Note that it wants a *source* upload, so the
build has to work from the packaged source alone.

**The Debian archive** is the heaviest path of the three families. It needs an
ITP bug against `wnpp`, a sponsor to upload for you until you are a Debian
Maintainer, and — for Rust — compliance with the Debian Rust team's policy,
which packages every crate as its own source package. `debian/rules` here is
deliberately a plain `cargo build`, which is fine for a PPA or a personal
repository and **not** what the archive expects.

## One project, every distribution: OBS

The [openSUSE Build Service](https://build.opensuse.org) builds `.deb` and
`.rpm` for Debian, Ubuntu, Fedora, RHEL, openSUSE and Arch from a single
project, and publishes ready-made repositories for each. If the goal is "users
on any distribution can install it", this costs less than maintaining three
pipelines — feed it the spec file and the `debian/` directory from here.

## The paths that need no packaging at all

Worth stating, because for a young project they carry most of the users:

```sh
cargo install hyprdmc                     # once published to crates.io
```

and a GitHub Release with prebuilt `x86_64` / `aarch64` binaries, which the AUR
`-bin` package convention can then wrap for people who do not want to compile
Rust.

## Vendoring

Both the RPM and the Debian package build from vendored crates, which is what
Fedora's builders, Launchpad and OBS require — none of them let a build reach
crates.io.

```sh
./packaging/vendor.sh 0.1.0      # -> hyprdmc-0.1.0-vendor.tar.xz (~16 MB)
```

`--locked` is not optional there: the tarball has to match `Cargo.lock`
exactly, or the offline build inside the package silently resolves something
else. The RPM takes it as `Source1`; the Debian build unpacks it next to a
`.cargo/config.toml` pointing at `vendor/`.

For Fedora, every bundled crate must be declared so that a CVE in a dependency
can find this package: `%cargo_vendor_manifest` in the spec generates that list
at build time.
