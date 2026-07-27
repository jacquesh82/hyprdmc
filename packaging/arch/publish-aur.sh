#!/usr/bin/env bash
# Publishes packaging/arch/PKGBUILD to the AUR.
#
# An AUR repository is not this repository: it holds the recipe only — PKGBUILD,
# .SRCINFO, and nothing else. This script builds that repository in a temporary
# directory from the PKGBUILD kept here, so there is one source of truth and no
# second copy to drift.
#
# Prerequisites, both outside what this script can do for you:
#   1. The tag must exist on the forge. `source=` points at it, and the checksum
#      is computed from what it downloads.
#   2. Your AUR SSH key must be registered on your AUR account
#      (https://aur.archlinux.org/account → "SSH Public Key").
#      Check with: ssh aur@aur.archlinux.org help
#
# Usage: packaging/arch/publish-aur.sh [--dry-run]

set -euo pipefail

pkgname=hyprdmc
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
dry_run=${1:-}

pkgver=$(sed -n 's/^pkgver=//p' "$here/PKGBUILD")
tag="v$pkgver"

step() { printf '\n\033[1m==> %s\033[0m\n' "$1"; }

step "Checking the tag $tag is published"
url=$(sed -n 's/^url="\(.*\)"/\1/p' "$here/PKGBUILD")
if ! git ls-remote --exit-code --tags "$url.git" "refs/tags/$tag" >/dev/null 2>&1; then
  echo "error: $tag is not on the remote. The AUR would download a 404." >&2
  echo "       git tag -a $tag -m '...' && git push origin $tag" >&2
  exit 1
fi

step "Checking the AUR accepts your key"
if ! ssh -o BatchMode=yes -o ConnectTimeout=10 aur@aur.archlinux.org help >/dev/null 2>&1; then
  echo "error: aur.archlinux.org refused the key in ~/.ssh/aur." >&2
  echo "       Register ~/.ssh/aur.pub at https://aur.archlinux.org/account" >&2
  exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

step "Cloning the AUR repository (created on first push)"
# A brand-new package has an empty repository: clone warns and gives you nothing,
# which is expected, so it must not take the script down with it.
git clone "ssh://aur@aur.archlinux.org/$pkgname.git" "$work/aur" 2>&1 | sed 's/^/    /' || true
cd "$work/aur"
git init -q 2>/dev/null || true
git remote add origin "ssh://aur@aur.archlinux.org/$pkgname.git" 2>/dev/null || true

cp "$here/PKGBUILD" .
cat > .gitignore <<'EOF'
# An AUR repository holds the recipe, never its output or its inputs.
*
!.gitignore
!PKGBUILD
!.SRCINFO
EOF

step "Computing the checksum from the published tarball"
updpkgsums
grep sha256sums PKGBUILD

step "Generating .SRCINFO"
# Mandatory and must be committed: the AUR reads package metadata from this file,
# not from the PKGBUILD, which it never executes.
makepkg --printsrcinfo > .SRCINFO

step "What would be pushed"
git add -A
git -c color.status=always status --short
git --no-pager diff --cached --stat

if [[ "$dry_run" == "--dry-run" ]]; then
  step "Dry run: stopping before the commit and the push"
  echo "    Repository left in $work/aur (removed on exit)."
  exit 0
fi

step "Committing and pushing"
git commit -qm "$pkgname $pkgver-1: initial import"
# The AUR only accepts master, whatever your own default branch is called.
git branch -M master
git push -u origin master

step "Done"
echo "    https://aur.archlinux.org/packages/$pkgname"
echo "    Copy the checksum back into packaging/arch/PKGBUILD:"
grep sha256sums PKGBUILD
