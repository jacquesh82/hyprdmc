# Packaging

Definitions for the three families, plus what it actually takes to get each one
into a repository people can install from.

| Family | Files | Built and verified here |
|---|---|---|
| Arch | [`arch/PKGBUILD`](arch/PKGBUILD) | **yes** — `makepkg` produced a 2.0 MB package, tests included |
| Fedora / RHEL | [`rpm/hyprdmc.spec`](rpm/hyprdmc.spec) | no — `rpmbuild` is not installed on the machine this was written on |
| Debian / Ubuntu | [`debian/`](debian/) | no — `dpkg-buildpackage` likewise |
| any | [`systemd/hyprdmc.service`](systemd/hyprdmc.service) | reference user unit, installed by all three |

All three install the same two things: `/usr/bin/hyprdmc`, and the systemd
**user** unit in `/usr/lib/systemd/user/`. Nothing runs as root — the daemon
drives one login session's displays and talks to that session's socket.

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

## Vendoring, if a build host has no network

Both the RPM spec and `debian/rules` can build offline when the source ships
its dependencies:

```sh
mkdir -p .cargo && cargo vendor > .cargo/config.toml
tar czf hyprdmc-0.1.0-vendor.tar.gz vendor .cargo
```

Add that as a second `Source` and pass `--offline` to `cargo build`. Some build
services (Launchpad, and OBS by default) require it.
