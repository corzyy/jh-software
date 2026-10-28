Name:           jh-software
Version:        0.1.0
Release:        1%{?dist}
Summary:        Software store for RPM, Terra and Flatpak
License:        GPL-3.0-or-later
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  make
BuildRequires:  pkgconfig(gtk4)
BuildRequires:  pkgconfig(libadwaita-1)
BuildRequires:  pkgconfig(flatpak)
BuildRequires:  desktop-file-utils

Requires:       PackageKit
Requires:       flatpak
Requires:       dnf5
Requires:       rpm
Recommends:     appstream-data

%description
A fast GTK 4 / libadwaita software store written in Rust. It installs,
updates and removes apps from Fedora RPM repositories, the Terra repository
and Flatpak remotes, preferring native packages over Flatpak.

%prep
%autosetup

%build
cargo build --release --locked

%install
%make_install PREFIX=%{_prefix}

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/io.github.corzyy.JHSoftware.desktop

%files
%{_bindir}/jh-software
%{_datadir}/applications/io.github.corzyy.JHSoftware.desktop
%{_datadir}/metainfo/io.github.corzyy.JHSoftware.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/io.github.corzyy.JHSoftware.svg
%{_datadir}/icons/hicolor/symbolic/apps/io.github.corzyy.JHSoftware-symbolic.svg

%changelog
* Wed Oct 28 2026 jh - 0.1.0-1
- Initial package
