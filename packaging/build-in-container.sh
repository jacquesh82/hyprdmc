#!/usr/bin/env bash
# Builds the .deb and the .rpm inside containers of the target distributions.
#
# This is not a convenience: a .deb built on an Arch machine links against
# Arch's glibc and would break on Debian. The package has to be produced in the
# distribution it is for. Containers also mean nothing is installed on the
# machine running this — no rpmbuild, no debhelper, no sudo.
#
#   ./packaging/build-in-container.sh            # both
#   ./packaging/build-in-container.sh deb        # one of them
#
# Output lands in dist/.
set -euo pipefail

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
dist="$root/dist"
runtime="$(command -v podman || command -v docker)"
trap 'rm -rf "$work"' EXIT

cd "$root"
mkdir -p "$dist"

# The sources exactly as a release tarball would contain them.
git archive --format=tar.gz --prefix="hyprdmc-$version/" -o "$work/hyprdmc-$version.tar.gz" HEAD

# Vendored crates: both builds run offline, which is what Fedora's builders,
# Launchpad and OBS require anyway.
[ -f "hyprdmc-$version-vendor.tar.xz" ] || ./packaging/vendor.sh "$version"
cp "hyprdmc-$version-vendor.tar.xz" "$work/"

build_rpm() {
  echo "==> RPM (fedora:42)"
  cp packaging/rpm/hyprdmc.spec "$work/"
  cat > "$work/rpm.sh" <<'INNER'
set -eux
dnf -q -y install rpm-build rpmdevtools cargo-rpm-macros rust gcc systemd-rpm-macros rpmlint
rpmdev-setuptree
cp /work/*.tar.gz /work/*.tar.xz "$HOME/rpmbuild/SOURCES/"
rpmbuild -ba /work/hyprdmc.spec
find "$HOME/rpmbuild/RPMS" -name '*.rpm' -exec cp {} /dist/ \;
rpmlint "$HOME"/rpmbuild/RPMS/*/*.rpm || true
INNER
  "$runtime" run --rm -v "$work:/work:ro" -v "$dist:/dist:rw" \
    docker.io/library/fedora:42 bash /work/rpm.sh
}

build_deb() {
  # Debian *stable* still ships rustc 1.85 and this crate needs 1.88, so the
  # build targets unstable — which is where new packages enter Debian anyway.
  echo "==> DEB (debian:sid)"
  mkdir -p "$work/deb"
  tar xzf "$work/hyprdmc-$version.tar.gz" -C "$work/deb"
  cp -r packaging/debian "$work/deb/hyprdmc-$version/"
  tar xf "$work/hyprdmc-$version-vendor.tar.xz" -C "$work/deb/hyprdmc-$version"
  mkdir -p "$work/deb/hyprdmc-$version/.cargo"
  printf '[source.crates-io]\nreplace-with = "vendored-sources"\n[source.vendored-sources]\ndirectory = "vendor"\n' \
    > "$work/deb/hyprdmc-$version/.cargo/config.toml"
  cat > "$work/deb/build.sh" <<INNER
set -eux
export DEBIAN_FRONTEND=noninteractive
apt-get -qq update
apt-get -qq install -y build-essential debhelper cargo rustc pkg-config lintian
cd /build/hyprdmc-$version
dpkg-buildpackage -us -uc -b
cp /build/*.deb /dist/
lintian /build/*.deb || true
INNER
  "$runtime" run --rm -v "$work/deb:/build:rw" -v "$dist:/dist:rw" \
    docker.io/library/debian:sid-slim bash /build/build.sh
}

case "${1:-all}" in
  rpm) build_rpm ;;
  deb) build_deb ;;
  all) build_rpm; build_deb ;;
  *) echo "usage: $0 [rpm|deb|all]" >&2; exit 2 ;;
esac

echo
echo "==> dist/"
ls -1 "$dist"
