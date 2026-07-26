# Fedora / RHEL spec, following the Fedora Rust Packaging Guidelines for a
# *binary application with vendored dependencies*:
#
#   - the %%cargo_* macros from rust-packaging drive the build, rather than a
#     bare `cargo build`, so the package inherits Fedora's flags, its offline
#     registry setup and its %%{_smp_mflags} handling;
#   - the crates are vendored into a second source tarball, which is what the
#     guidelines allow for leaf applications (as opposed to rust2rpm, one RPM
#     per crate, which is meant for libraries other packages build against);
#   - every bundled crate is declared, so `dnf repoquery --whatprovides
#     'bundled(crate(...))'` finds this package when a dependency needs a CVE
#     fix. %%cargo_vendor_manifest generates that list at build time.
#
# Build it with:
#   spectool -g -R packaging/rpm/hyprdmc.spec     # fetch both sources
#   rpmbuild -ba packaging/rpm/hyprdmc.spec
#
# See packaging/README.md for how the vendor tarball is produced and for the
# COPR route.

%global crate hyprdmc

Name:           hyprdmc
Version:        0.1.0
Release:        1%{?dist}
Summary:        Dynamic monitor configuration for Hyprland

License:        MIT
URL:            https://github.com/jacquesh82/hyprdmc
Source0:        %{url}/archive/refs/tags/v%{version}/%{name}-%{version}.tar.gz
# Produced by `packaging/vendor.sh` — see packaging/README.md.
Source1:        %{name}-%{version}-vendor.tar.xz

BuildRequires:  cargo-rpm-macros >= 24
BuildRequires:  rust >= 1.88
BuildRequires:  gcc
BuildRequires:  systemd-rpm-macros

# The web UI reads the xkb catalogue from this package. Without it the layout
# list falls back to a short built-in one and everything still works, so this
# is a Recommends rather than a Requires.
Recommends:     xkeyboard-config
Recommends:     libnotify
Suggests:       hyprland

# Rust upstream ships these two; nothing else here is architecture-specific.
ExclusiveArch:  %{rust_arches}

%description
hyprdmc detects Hyprland outputs and positions, rotates, flips or mirrors them
from a command line or a browser. A daemon watches for hotplug events and
reapplies the profile that matches the screens actually plugged in, with an
automatic rollback if a change is not confirmed.

It also configures the keyboard layout and the pointer scroll direction,
deliberately kept apart from the screen profiles: docking a laptop must not
change what you type in.

%prep
%autosetup -n %{name}-%{version} -a1
# -v vendor: build from the vendored crates in Source1, offline.
%cargo_prep -v vendor

%generate_buildrequires
# Nothing to emit: the dependencies are vendored, not resolved from Fedora
# packages. Kept explicit so the omission does not read as an oversight.

%build
%cargo_build
%{cargo_license_summary}
%{cargo_license} > LICENSE.dependencies
%{cargo_vendor_manifest}

%install
%cargo_install
# %%cargo_install also copies the crate sources into %%{cargo_registry}, because
# this crate has a lib target next to its binary. That library exists so the
# binary and the tests can share code, not for other packages to build against,
# so shipping it as a -devel subpackage would be noise — and leaving the files
# unpackaged is a hard rpmbuild error.
rm -rf %{buildroot}%{cargo_registry}
# A *user* unit: the daemon drives one login session's displays and talks to
# that session's socket, so it must not run as root. %%{_userunitdir} is
# /usr/lib/systemd/user.
install -Dpm0644 packaging/systemd/%{name}.service \
    %{buildroot}%{_userunitdir}/%{name}.service

%check
%cargo_test

%files
%license LICENSE
%license LICENSE.dependencies
%doc README.md
%doc cargo-vendor.txt
%{_bindir}/%{name}
%{_userunitdir}/%{name}.service

%changelog
* Sun Jul 26 2026 Jacques Hullu <jacques@hullu.fr> - 0.1.0-1
- Initial package.

# Fedora's own packages use rpmautospec (%%autorelease / %%autochangelog), which
# only resolves inside fedpkg. An explicit Release and changelog keep this spec
# buildable with a plain `rpmbuild -ba`, which is what COPR and anyone cloning
# the repository will do.
