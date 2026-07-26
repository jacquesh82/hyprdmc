#!/usr/bin/env bash
# Produces the vendored-crates tarball the RPM and the Debian package use to
# build without network access — a requirement on Fedora's builders, on
# Launchpad and on OBS by default.
#
#   ./packaging/vendor.sh 0.1.0   ->  hyprdmc-0.1.0-vendor.tar.xz
set -euo pipefail

version="${1:?usage: vendor.sh <version>}"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

# --locked: the tarball must match Cargo.lock exactly, or the offline build
# inside the package would silently resolve something else.
cargo vendor --locked vendor > /tmp/hyprdmc-vendor-config.toml

tar caf "hyprdmc-${version}-vendor.tar.xz" vendor
rm -rf vendor
echo "hyprdmc-${version}-vendor.tar.xz"
