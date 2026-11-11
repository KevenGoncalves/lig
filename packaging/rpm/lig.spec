# Copr build from source; .copr/Makefile fills in the version (from Cargo.toml) and date.
# Cargo fetches the crates, so the Copr project needs internet access during builds.
%global debug_package %{nil}

Name:           lig
Version:        @VERSION@
Release:        1%{?dist}
Summary:        Fig-style IDE autocomplete for your terminal
License:        MIT
URL:            https://github.com/KevenGoncalves/lig
Source0:        lig-%{version}.tar.gz

BuildRequires:  cargo >= 1.85
BuildRequires:  rust >= 1.85
BuildRequires:  gcc
Recommends:     curl

%description
lig shows Fig-style IDE autocomplete suggestions as you type in zsh, bash
and fish: subcommands, options, files, git branches and more, from over
740 commands.

%prep
%autosetup

%build
cargo build --release --locked

%install
install -Dm755 target/release/lig %{buildroot}%{_bindir}/lig

%post
if [ "$1" = 1 ]; then
    echo "lig: run 'lig install' as your user to turn on the popup in your shell."
fi

%files
%license LICENSE
%doc README.md
%{_bindir}/lig

%changelog
* @DATE@ Keven Gonçalves <keven.jm.goncalves@gmail.com> - @VERSION@-1
- lig @VERSION@: https://github.com/KevenGoncalves/lig/releases/tag/v@VERSION@
