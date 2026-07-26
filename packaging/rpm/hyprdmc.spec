# Fedora / RHEL / openSUSE spec. Build it with:
#   rpmbuild -ba packaging/rpm/hyprdmc.spec
# or push it to COPR, which builds for every Fedora and EPEL release at once.
#
# Fedora's own Rust packaging guidelines want each crate packaged separately
# (rust2rpm). That road is for packages entering the *official* repositories;
# for COPR and for personal builds, vendoring the dependencies into the source
# tarball is both allowed and far less work — see packaging/README.md.

Name:           hyprdmc
Version:        0.1.0
Release:        1%{?dist}
Summary:        Dynamic monitor configuration for Hyprland

License:        MIT
URL:            https://github.com/jacquesh82/hyprdmc
Source0:        %{url}/archive/refs/tags/v%{version}/%{name}-%{version}.tar.gz

BuildRequires:  cargo >= 1.87
BuildRequires:  rust >= 1.87
BuildRequires:  gcc
BuildRequires:  systemd-rpm-macros

# The web UI reads the xkb catalogue from this package; without it the layout
# list falls back to a short built-in one, so it is a Recommends, not a
# Requires — the tool still works.
Recommends:     xkeyboard-config
Recommends:     libnotify
Suggests:       hyprland

# Rust upstream only ships these; nothing here is architecture-specific
# beyond that.
ExclusiveArch:  x86_64 aarch64

%description
hyprdmc detects Hyprland outputs and positions, rotates, flips or mirrors them
from a command line or a browser. A daemon watches for hotplug events and
reapplies the profile that matches the screens actually plugged in, with an
automatic rollback if a change is not confirmed. It also configures the
keyboard layout and the pointer scroll direction, separately from the screen
profiles.

%prep
%autosetup -n %{name}-%{version}

%build
# --offline when the tarball ships a vendored .cargo directory; drop it for a
# build that is allowed to reach crates.io.
cargo build --release --locked

%check
cargo test --release --locked

%install
install -Dpm0755 target/release/%{name} %{buildroot}%{_bindir}/%{name}
# %%{_userunitdir} is /usr/lib/systemd/user: a *user* unit, since the daemon
# drives one login session's displays and must not run as root.
install -Dpm0644 packaging/systemd/%{name}.service \
    %{buildroot}%{_userunitdir}/%{name}.service

%files
%license LICENSE
%doc README.md
%{_bindir}/%{name}
%{_userunitdir}/%{name}.service

%changelog
* Sat Jul 26 2026 Jacques Hullu <jacques@hullu.fr> - 0.1.0-1
- Initial package.
