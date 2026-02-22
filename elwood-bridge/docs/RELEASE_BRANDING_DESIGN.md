# Elwood Terminal: Release Engineering & Branding Design

> Research document for cross-platform release builds and branding rename of the WezTerm
> fork into Elwood Terminal.

## Table of Contents

1. [WezTerm's Existing Build System](#1-wezterms-existing-build-system)
2. [Branding Rename Checklist](#2-branding-rename-checklist)
3. [macOS Release Engineering](#3-macos-release-engineering)
4. [Linux Release Engineering](#4-linux-release-engineering)
5. [Windows Release Engineering](#5-windows-release-engineering)
6. [GitHub Actions Workflows](#6-github-actions-workflows)
7. [Implementation Plan](#7-implementation-plan)

---

## 1. WezTerm's Existing Build System

### 1.1 Architecture Overview

WezTerm's CI uses **generated GitHub Actions workflows** (produced by `generate-workflows.py`)
with per-distro YAML files. The workspace builds four main binaries:

| Binary               | Crate                  | Purpose                                |
|----------------------|------------------------|----------------------------------------|
| `wezterm`            | `wezterm/`             | CLI launcher (shell completion, etc.)  |
| `wezterm-gui`        | `wezterm-gui/`         | Main GUI terminal (GPU-rendered)       |
| `wezterm-mux-server` | `wezterm-mux-server/`  | Headless multiplexer server            |
| `strip-ansi-escapes` | `strip-ansi-escapes/`  | ANSI escape sequence stripper utility  |

For Elwood Terminal, the primary binary has already been renamed to `elwood` in
`wezterm-gui/Cargo.toml` (`[[bin]] name = "elwood"`), but the surrounding
infrastructure still uses WezTerm naming.

### 1.2 Build Toolchain

- **Compiler caching**: `sccache` via `mozilla-actions/sccache-action@v0.0.9`
  with `SCCACHE_GHA_ENABLED=true` and `RUSTC_WRAPPER=sccache`
- **Dependency caching**: `cargo vendor --locked --versioned-dirs` with GitHub
  Actions `actions/cache@v4` keyed on `Cargo.lock` hash
- **Testing**: `cargo-nextest` for parallel test execution
- **Packaging**: `ci/deploy.sh` handles all platform-specific packaging
- **Version**: Derived from git commit (`%Y%m%d-%H%M%S-%h`) or `.tag` file override
  via `wezterm-version/build.rs` (sets `WEZTERM_CI_TAG` and `WEZTERM_TARGET_TRIPLE`)

### 1.3 Per-Platform Dependencies

#### macOS
- No system dependencies needed (freetype/harfbuzz vendored in `deps/`)
- Targets: `x86_64-apple-darwin` + `aarch64-apple-darwin` (universal binary via `lipo`)
- `MACOSX_DEPLOYMENT_TARGET=10.12`

#### Linux (Debian/Ubuntu)
```
cmake dpkg-dev fakeroot gcc g++ libegl1-mesa-dev libssl-dev libfontconfig1-dev
libwayland-dev libx11-xcb-dev libxcb-ewmh-dev libxcb-icccm4-dev libxcb-image0-dev
libxcb-keysyms1-dev libxcb-randr0-dev libxcb-render0-dev libxcb-xkb-dev
libxkbcommon-dev libxkbcommon-x11-dev libxcb-util0-dev
```

#### Linux (Fedora/CentOS)
```
gcc gcc-c++ make fontconfig-devel openssl-devel perl-interpreter python3
libxcb-devel libxkbcommon-devel libxkbcommon-x11-devel wayland-devel
mesa-libEGL-devel xcb-util-devel xcb-util-keysyms-devel xcb-util-image-devel
xcb-util-wm-devel rpm-build
```
Note: Fedora 41+ needs `openssl-devel-engine`.

#### Windows
- Perl (Strawberry Perl on PATH for OpenSSL)
- Target: `x86_64-pc-windows-msvc`
- Build-time: `cc`, `embed-resource` crates for `.rc` resource compilation
- Runtime DLLs: `conpty.dll`, `OpenConsole.exe`, `libEGL.dll`, `libGLESv2.dll`, `mesa/opengl32.dll`

### 1.4 Generated Workflow Structure

WezTerm generates **3 variants** per platform:
- `gen_{platform}.yml` -- PR builds (on pull_request to main)
- `gen_{platform}_continuous.yml` -- Nightly builds (on schedule)
- `gen_{platform}_tag.yml` -- Release builds (on tag push matching `20*`)

Platforms with workflows: macOS, Windows, Ubuntu 20.04/22.04/24.04, Debian 11/12,
Fedora 39/40/41, CentOS 9, plus Nix and Flatpak.

---

## 2. Branding Rename Checklist

### 2.1 Already Done (in current fork)

| Item | File | Status |
|------|------|--------|
| Binary name `elwood` | `wezterm-gui/Cargo.toml` `[[bin]]` | DONE |
| Feature flag `elwood` | `wezterm-gui/Cargo.toml` features | DONE |
| Config path `~/.elwood/config.lua` | `config/src/config.rs:1009` | DONE |
| XDG config `elwood/config.lua` | `config/src/config.rs:1011` | DONE |
| Windows portable `elwood/config.lua` | `config/src/config.rs:1025` | DONE |
| Lua module path `~/.elwood` | `config/src/lua.rs:230` | DONE |
| Workspace member `elwood-bridge` | Root `Cargo.toml` | DONE |

### 2.2 Remaining Branding Changes

#### Binary Names
| Current | New | File(s) |
|---------|-----|---------|
| `wezterm` (CLI) | `elwood-cli` | `wezterm/Cargo.toml` `[[bin]]` (if separate binary is kept) |
| `wezterm-mux-server` | `elwood-mux-server` | `wezterm-mux-server/Cargo.toml` |
| `strip-ansi-escapes` | Keep as-is (generic utility) | - |

#### macOS Bundle
| Item | Current | New | File |
|------|---------|-----|------|
| Bundle executable | `wezterm-gui` | `elwood` | `assets/macos/WezTerm.app/Contents/Info.plist` line 8 |
| Bundle identifier | `com.github.wez.wezterm` | `io.elwood.terminal` | Info.plist line 10 |
| Bundle name | `WezTerm` | `Elwood Terminal` | Info.plist line 14 |
| Display name | `WezTerm` | `Elwood Terminal` | Info.plist line 36 |
| Icon file | `terminal.icns` | `elwood.icns` (new icon needed) | Info.plist line 26 |
| App dir name | `WezTerm.app` | `Elwood Terminal.app` | `assets/macos/` directory |
| Usage strings (x12) | "...via WezTerm..." | "...via Elwood Terminal..." | Info.plist lines 40-63 |
| Entitlements | Same | Same (no branding) | `ci/macos-entitlement.plist` |
| Saved state path | `com.github.wez.wezterm.savedState` | `io.elwood.terminal.savedState` | Homebrew template |

#### Linux Desktop Integration
| Item | Current | New | File |
|------|---------|-----|------|
| Desktop entry name | `WezTerm` | `Elwood Terminal` | `assets/wezterm.desktop` |
| Comment | `Wez's Terminal Emulator` | `AI-native terminal` | `assets/wezterm.desktop` |
| Icon ID | `org.wezfurlong.wezterm` | `io.elwood.terminal` | `assets/wezterm.desktop` |
| StartupWMClass | `org.wezfurlong.wezterm` | `io.elwood.terminal` | `assets/wezterm.desktop` |
| TryExec/Exec | `wezterm` | `elwood` | `assets/wezterm.desktop` |
| AppData ID | `org.wezfurlong.wezterm` | `io.elwood.terminal` | `assets/wezterm.appdata.xml` |
| AppData name | `Wez's Terminal Emulator` | `Elwood Terminal` | `assets/wezterm.appdata.xml` |
| Desktop file name | `wezterm.desktop` | `elwood.desktop` | Rename file |
| AppData file name | `wezterm.appdata.xml` | `elwood.appdata.xml` | Rename file |
| Nautilus plugin | `wezterm-nautilus.py` | `elwood-nautilus.py` | Rename + update |
| Icon file (PNG) | `terminal.png` | `elwood.png` (new icon) | `assets/icon/` |

#### Windows Resources
| Item | Current | New | File |
|------|---------|-----|------|
| CompanyName | `Wez Furlong` | `Elwood Project` | `wezterm-gui/build.rs` line 122 |
| FileDescription | `WezTerm - Wez's Terminal Emulator` | `Elwood Terminal` | build.rs line 123 |
| ProductName | `WezTerm` | `Elwood Terminal` | build.rs line 127 |
| Icon | `terminal.ico` | `elwood.ico` (new) | `assets/windows/terminal.ico` |
| Installer AppName | `WezTerm` | `Elwood Terminal` | `ci/windows-installer.iss` line 5 |
| Installer AppId | `{BCF6F0DA-...}` | New GUID | `ci/windows-installer.iss` line 12 |
| AppPublisher | `Wez Furlong` | `Elwood Project` | `ci/windows-installer.iss` line 7 |
| AppURL | `http://wezterm.org` | `https://elwood.io` (or equivalent) | `ci/windows-installer.iss` |
| Context menu | `Open WezTerm here` | `Open Elwood here` | `ci/windows-installer.iss` lines 65-73 |
| AppUserModelID | `org.wezfurlong.wezterm` | `io.elwood.terminal` | `ci/windows-installer.iss` lines 58-59 |

#### Environment Variables
| Current | New | Files Affected |
|---------|-----|----------------|
| `WEZTERM_EXECUTABLE` | `ELWOOD_EXECUTABLE` | `env-bootstrap/src/lib.rs` |
| `WEZTERM_EXECUTABLE_DIR` | `ELWOOD_EXECUTABLE_DIR` | `env-bootstrap/src/lib.rs` |
| `WEZTERM_CONFIG_FILE` | `ELWOOD_CONFIG_FILE` | `config/src/config.rs`, `config/src/lib.rs`, `env-bootstrap/src/lib.rs` |
| `WEZTERM_CONFIG_DIR` | `ELWOOD_CONFIG_DIR` | `config/src/config.rs`, `config/src/lib.rs` |
| `WEZTERM_UNIX_SOCKET` | `ELWOOD_UNIX_SOCKET` | `mux/src/domain.rs`, `wezterm-client/src/client.rs`, `wezterm-mux-server/src/main.rs` |
| `WEZTERM_CI_TAG` | `ELWOOD_CI_TAG` | `wezterm-version/build.rs`, `wezterm-version/src/lib.rs` |
| `WEZTERM_TARGET_TRIPLE` | `ELWOOD_TARGET_TRIPLE` | `wezterm-version/build.rs`, `wezterm-version/src/lib.rs` |

**Backward compatibility**: Add fallback reads from the old `WEZTERM_*` vars with a deprecation
warning for one major version cycle. For config files, already done: `~/.elwood/config.lua`
is checked first, with XDG fallback.

#### Shell Integration
| Current | New | File |
|---------|-----|------|
| `wezterm.sh` | `elwood.sh` | `assets/shell-integration/wezterm.sh` |
| `WEZTERM_SHELL_SKIP_ALL` | `ELWOOD_SHELL_SKIP_ALL` | shell integration script |
| `WEZTERM_SHELL_SKIP_*` (4 vars) | `ELWOOD_SHELL_SKIP_*` | shell integration script |
| `WEZTERM_PROG` user var | `ELWOOD_PROG` | shell integration script |
| `WEZTERM_USER` user var | `ELWOOD_USER` | shell integration script |
| `WEZTERM_IN_TMUX` user var | `ELWOOD_IN_TMUX` | shell integration script |
| `WEZTERM_HOST` user var | `ELWOOD_HOST` | shell integration script |
| `WEZTERM_HOSTNAME` | `ELWOOD_HOSTNAME` | shell integration script |
| `__wezterm_*` functions | `__elwood_*` | shell integration script |
| Shell completions (bash/zsh/fish) | Update binary name | `assets/shell-completion/*` |
| Terminfo entry `wezterm` | `elwood` | `termwiz/data/wezterm.terminfo` |

#### Version System
| Current | New | File |
|---------|-----|------|
| `wezterm_version()` | `elwood_version()` | `wezterm-version/src/lib.rs` |
| `wezterm_target_triple()` | `elwood_target_triple()` | `wezterm-version/src/lib.rs` |
| Version format `%Y%m%d-%H%M%S-%h` | Semver `0.1.0-{git_hash}` | Recommended change |

#### Data Directories
| Current | New | File |
|---------|-----|------|
| `~/.local/share/wezterm` | `~/.local/share/elwood` | `config/src/config.rs:1745-1761` |
| XDG config `wezterm/` | `elwood/` | `config/src/lib.rs:386-390` |

### 2.3 Migration Strategy

1. **Config files**: Check `~/.elwood/config.lua` first (already done), then fall back to
   `~/.config/wezterm/wezterm.lua` with a one-time migration prompt.
2. **Env vars**: Read `ELWOOD_*` first, fall back to `WEZTERM_*` with deprecation log warning.
3. **Data dirs**: On first launch, copy `~/.local/share/wezterm/` to `~/.local/share/elwood/`.
4. **Socket paths**: `ELWOOD_UNIX_SOCKET` default to `/tmp/elwood-mux-$USER`.

---

## 3. macOS Release Engineering

### 3.1 Universal Binary Build

WezTerm already builds universal binaries. The process:

```bash
# Build for both architectures
cargo build --target x86_64-apple-darwin -p wezterm-gui --release --features elwood
cargo build --target aarch64-apple-darwin -p wezterm-gui --release --features elwood

# Combine via lipo
lipo target/x86_64-apple-darwin/release/elwood \
     target/aarch64-apple-darwin/release/elwood \
     -output "Elwood Terminal.app/Contents/MacOS/elwood" \
     -create
```

Minimum deployment target: `MACOSX_DEPLOYMENT_TARGET=10.15` (raise from 10.12 for
modern GPU/Metal requirements; WezTerm's 10.12 is legacy).

### 3.2 .app Bundle Structure

```
Elwood Terminal.app/
  Contents/
    Info.plist              # Bundle metadata (see Section 2.2)
    MacOS/
      elwood                # Main GUI binary (universal)
      elwood-cli            # CLI launcher (universal)
      elwood-mux-server     # Mux server (universal)
      strip-ansi-escapes    # Utility (universal)
    Resources/
      elwood.icns           # Application icon
      terminfo/             # Compiled terminfo entries
      shell-integration/    # Shell integration scripts
      shell-completion/     # Bash/Zsh/Fish completions
```

### 3.3 Code Signing

WezTerm's existing `ci/deploy.sh` handles this with secrets:

```bash
# Required GitHub Actions secrets:
# MACOS_TEAM_ID    - Developer ID (e.g., "XXXXXXXXXX")
# MACOS_APPLEID    - Apple ID email for notarization
# MACOS_APP_PW     - App-specific password for notarization
# MACOS_CERT       - Base64-encoded .p12 certificate
# MACOS_CERT_PW    - Certificate password (base64-encoded)

# Signing command (from ci/deploy.sh):
/usr/bin/codesign --keychain build.keychain --force --options runtime \
  --entitlements ci/macos-entitlement.plist \
  --deep --sign "$MACOS_TEAM_ID" \
  "Elwood Terminal.app/"
```

For open-source ad-hoc signing (no Apple Developer account):
```bash
codesign --force --deep --sign - "Elwood Terminal.app/"
```

### 3.4 Notarization

```bash
xcrun notarytool submit Elwood-Terminal-macos-${TAG}.zip \
  --wait \
  --team-id "$MACOS_TEAM_ID" \
  --apple-id "$MACOS_APPLEID" \
  --password "$MACOS_APP_PW"
```

### 3.5 DMG Creation (Enhancement over WezTerm)

WezTerm distributes as `.zip`. We can add DMG for better UX:

```bash
# Using create-dmg (npm install -g create-dmg or brew install create-dmg)
create-dmg \
  --volname "Elwood Terminal" \
  --volicon "assets/macos/elwood.icns" \
  --window-pos 200 120 \
  --window-size 600 400 \
  --icon-size 100 \
  --icon "Elwood Terminal.app" 175 190 \
  --hide-extension "Elwood Terminal.app" \
  --app-drop-link 425 190 \
  --no-internet-enable \
  "Elwood-Terminal-${TAG}.dmg" \
  "staging/"
```

### 3.6 Homebrew Cask Template

```ruby
# ci/elwood-homebrew-macos.rb.template
cask "elwood-terminal" do
  version "@TAG@"
  sha256 "@SHA256@"

  url "https://github.com/gordonwatts/elwood-pro/releases/download/#{version}/Elwood-Terminal-macos-#{version}.zip"
  name "Elwood Terminal"
  desc "AI-native terminal emulator with GPU rendering, forked from WezTerm"
  homepage "https://elwood.io/"  # or GitHub URL

  app "Elwood Terminal.app"
  [
    "elwood",
    "elwood-cli",
    "elwood-mux-server",
    "strip-ansi-escapes"
  ].each do |tool|
    binary "#{appdir}/Elwood Terminal.app/Contents/MacOS/#{tool}"
  end

  preflight do
    staged_subfolder = staged_path.glob(["Elwood-Terminal-*", "elwood-terminal-*"]).first
    if staged_subfolder
      FileUtils.mv(staged_subfolder/"Elwood Terminal.app", staged_path)
      FileUtils.rm_rf(staged_subfolder)
    end
  end

  zap trash: [
    "~/Library/Saved Application State/io.elwood.terminal.savedState",
    "~/.elwood",
  ]
end
```

---

## 4. Linux Release Engineering

### 4.1 .deb Package

WezTerm's existing `ci/deploy.sh` handles Debian packaging. Key changes for Elwood:

```
Package: elwood-terminal
Version: ${VERSION}
Architecture: amd64
Maintainer: Gordon Watts <...>
Section: utils
Priority: optional
Homepage: https://elwood.io/
Description: Elwood Terminal - AI-native terminal emulator.
 Elwood Terminal is a GPU-accelerated terminal with built-in AI agent support,
 forked from WezTerm. Features include font ligatures, hyperlinks, tabs,
 split panes, and an integrated coding agent.
Provides: x-terminal-emulator
Depends: ${shlibs:Depends}
```

File layout:
```
/usr/bin/elwood
/usr/bin/elwood-cli
/usr/bin/elwood-mux-server
/usr/bin/open-elwood-here
/usr/bin/strip-ansi-escapes
/usr/share/applications/io.elwood.terminal.desktop
/usr/share/icons/hicolor/128x128/apps/io.elwood.terminal.png
/usr/share/metainfo/io.elwood.terminal.appdata.xml
/etc/profile.d/elwood.sh
/usr/share/bash-completion/completions/elwood
/usr/share/zsh/functions/Completion/Unix/_elwood
```

Build command:
```bash
fakeroot dpkg-deb --build pkg/debian elwood-terminal-${VERSION}.${DISTRO}${DISTVER}.deb
```

### 4.2 .rpm Package

```spec
Name: elwood-terminal
Version: ${VERSION}
Release: 1.${distroid}${distver}
Packager: Gordon Watts
License: MIT
URL: https://elwood.io/
Summary: Elwood Terminal - AI-native terminal emulator.
Requires: openssl, dbus, fontconfig, libxcb, libxkbcommon, libxkbcommon-x11,
          libwayland-client, libwayland-egl, mesa-libEGL, xcb-util-keysyms, xcb-util-wm

%description
Elwood Terminal is a GPU-accelerated terminal with built-in AI agent support.
```

### 4.3 AppImage

```bash
# Download linuxdeploy
wget https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage
chmod +x linuxdeploy-x86_64.AppImage

# Create AppDir
mkdir -p Elwood-Terminal.AppDir/usr/bin
mkdir -p Elwood-Terminal.AppDir/usr/share/applications
mkdir -p Elwood-Terminal.AppDir/usr/share/icons/hicolor/128x128/apps

cp target/release/elwood Elwood-Terminal.AppDir/usr/bin/
cp target/release/elwood-mux-server Elwood-Terminal.AppDir/usr/bin/
cp assets/elwood.desktop Elwood-Terminal.AppDir/usr/share/applications/
cp assets/icon/elwood.png Elwood-Terminal.AppDir/usr/share/icons/hicolor/128x128/apps/

# Build AppImage (auto-detects and bundles shared libraries)
./linuxdeploy-x86_64.AppImage \
  --appdir Elwood-Terminal.AppDir \
  --desktop-file assets/elwood.desktop \
  --icon-file assets/icon/elwood.png \
  --output appimage

# Result: Elwood_Terminal-x86_64.AppImage
```

Note: GPU-dependent apps may need `--plugin gtk` or custom library exclusion lists
since users should use their own Mesa/GPU drivers.

### 4.4 Flatpak Manifest

```json
{
    "app-id": "io.elwood.terminal",
    "runtime": "org.freedesktop.Platform",
    "runtime-version": "24.08",
    "sdk": "org.freedesktop.Sdk",
    "sdk-extensions": ["org.freedesktop.Sdk.Extension.rust-stable"],
    "command": "elwood",
    "finish-args": [
        "--share=ipc",
        "--filesystem=home:ro",
        "--filesystem=xdg-config/elwood",
        "--socket=fallback-x11",
        "--socket=wayland",
        "--device=dri",
        "--talk-name=org.freedesktop.Flatpak",
        "--talk-name=org.freedesktop.Notifications",
        "--share=network"
    ],
    "modules": [
        {
            "name": "elwood-terminal",
            "buildsystem": "simple",
            "build-options": {
                "append-path": "/usr/lib/sdk/rust-stable/bin",
                "env": { "CARGO_HOME": "/run/build/elwood-terminal/cargo" }
            },
            "build-commands": [
                "cargo --offline build --release --features elwood",
                "install -Dm755 ./target/release/elwood -t /app/bin/",
                "install -Dm755 ./target/release/elwood-mux-server -t /app/bin/",
                "install -Dm644 ./assets/icon/elwood.png /app/share/icons/hicolor/128x128/apps/io.elwood.terminal.png",
                "install -Dm644 ./assets/elwood.desktop /app/share/applications/io.elwood.terminal.desktop",
                "install -Dm644 ./assets/elwood.appdata.xml /app/share/metainfo/io.elwood.terminal.appdata.xml"
            ]
        }
    ]
}
```

### 4.5 AUR PKGBUILD (Arch Linux)

```bash
# Maintainer: Gordon Watts
pkgname=elwood-terminal-bin
pkgver=0.1.0
pkgrel=1
pkgdesc='AI-native GPU-accelerated terminal emulator'
arch=('x86_64')
url='https://github.com/gordonwatts/elwood-pro'
license=('MIT')
depends=('fontconfig' 'libxcb' 'libxkbcommon' 'libxkbcommon-x11' 'wayland'
         'xcb-util' 'xcb-util-image' 'xcb-util-keysyms' 'xcb-util-wm'
         'mesa' 'openssl' 'dbus')
provides=('elwood-terminal')
source=("${url}/releases/download/v${pkgver}/Elwood-Terminal-linux-${pkgver}.tar.xz")
sha256sums=('SKIP')

package() {
    install -Dm755 -t "${pkgdir}/usr/bin/" usr/bin/elwood
    install -Dm755 -t "${pkgdir}/usr/bin/" usr/bin/elwood-mux-server
    install -Dm755 -t "${pkgdir}/usr/bin/" usr/bin/strip-ansi-escapes
    install -Dm644 -t "${pkgdir}/usr/share/applications/" usr/share/applications/io.elwood.terminal.desktop
    install -Dm644 -t "${pkgdir}/usr/share/icons/hicolor/128x128/apps/" usr/share/icons/hicolor/128x128/apps/io.elwood.terminal.png
    install -Dm644 -t "${pkgdir}/usr/share/metainfo/" usr/share/metainfo/io.elwood.terminal.appdata.xml
    install -Dm644 -t "${pkgdir}/etc/profile.d/" etc/profile.d/elwood.sh
}
```

---

## 5. Windows Release Engineering

### 5.1 Inno Setup Installer

Adapt from WezTerm's existing `ci/windows-installer.iss`:

```iss
#define MyAppName "Elwood Terminal"
#define MyAppPublisher "Elwood Project"
#define MyAppURL "https://elwood.io"
#define MyAppExeName "elwood.exe"

[Setup]
AppId={{NEW-GUID-HERE}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
SetupIconFile=..\assets\windows\elwood.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
MinVersion=10.0.17763

[Files]
Source: "..\target\release\elwood.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\elwood-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\elwood-mux-server.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\strip-ansi-escapes.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\conpty.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\OpenConsole.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\libEGL.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\libGLESv2.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\mesa\opengl32.dll"; DestDir: "{app}\mesa"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; AppUserModelID: "io.elwood.terminal"

[Registry]
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Open Elwood here"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Open Elwood here"; ValueName: "icon"; ValueType: string; ValueData: "{app}\{#MyAppExeName}"
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Open Elwood here\command"; ValueType: string; ValueData: """{app}\{#MyAppExeName}"" start --cwd ""%V"""
```

### 5.2 Windows Resource File (build.rs)

```rc
VALUE "CompanyName",      "Elwood Project\0"
VALUE "FileDescription",  "Elwood Terminal - AI-native terminal emulator\0"
VALUE "ProductName",      "Elwood Terminal\0"
VALUE "LegalCopyright",   "MIT License\0"
```

### 5.3 Windows Manifest

Already correct for basic DPI awareness / UTF-8 in `assets/windows/manifest.manifest`.
No branding changes needed.

### 5.4 winget Manifest

```yaml
# manifests/e/ElwoodProject/ElwoodTerminal/0.1.0/ElwoodProject.ElwoodTerminal.yaml
PackageIdentifier: ElwoodProject.ElwoodTerminal
PackageVersion: 0.1.0
PackageLocale: en-US
Publisher: Elwood Project
PackageName: Elwood Terminal
License: MIT
ShortDescription: AI-native GPU-accelerated terminal emulator
InstallerType: inno
Installers:
  - Architecture: x64
    InstallerUrl: https://github.com/gordonwatts/elwood-pro/releases/download/v0.1.0/Elwood-Terminal-Setup.exe
    InstallerSha256: <SHA256>
ManifestType: singleton
ManifestVersion: 1.6.0
```

### 5.5 Scoop Manifest

```json
{
    "version": "0.1.0",
    "description": "AI-native GPU-accelerated terminal emulator",
    "homepage": "https://github.com/gordonwatts/elwood-pro",
    "license": "MIT",
    "architecture": {
        "64bit": {
            "url": "https://github.com/gordonwatts/elwood-pro/releases/download/v0.1.0/Elwood-Terminal-windows-0.1.0.zip",
            "hash": "<SHA256>"
        }
    },
    "bin": ["elwood.exe", "elwood-cli.exe", "elwood-mux-server.exe"],
    "shortcuts": [["elwood.exe", "Elwood Terminal"]],
    "checkver": "github",
    "autoupdate": {
        "architecture": {
            "64bit": {
                "url": "https://github.com/gordonwatts/elwood-pro/releases/download/v$version/Elwood-Terminal-windows-$version.zip"
            }
        }
    }
}
```

---

## 6. GitHub Actions Workflows

### 6.1 Unified CI Workflow (Simplified from WezTerm)

Instead of WezTerm's ~40 generated workflows, use a single matrix-based approach:

```yaml
# .github/workflows/elwood-term-ci.yml
name: Elwood Terminal CI

on:
  pull_request:
    branches: [main]
    paths:
      - "elwood-term/wezterm/**"
      - ".github/workflows/elwood-term-*.yml"
  push:
    branches: [main]
    paths:
      - "elwood-term/wezterm/**"

jobs:
  build:
    strategy:
      fail-fast: false
      matrix:
        include:
          - os: macos-latest
            name: macOS
            target_intel: x86_64-apple-darwin
            target_arm: aarch64-apple-darwin
          - os: ubuntu-latest
            name: Linux
            container: ubuntu:24.04
          - os: windows-2022
            name: Windows
            target: x86_64-pc-windows-msvc

    runs-on: ${{ matrix.os }}
    container: ${{ matrix.container || '' }}
    defaults:
      run:
        working-directory: elwood-term/wezterm

    env:
      CARGO_INCREMENTAL: "0"
      SCCACHE_GHA_ENABLED: "true"
      RUSTC_WRAPPER: "sccache"

    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive

      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target_intel && format('{0},{1}', matrix.target_intel, matrix.target_arm) || matrix.target || '' }}

      - uses: mozilla-actions/sccache-action@v0.0.9

      - name: Cache Vendor
        uses: actions/cache@v4
        id: cache-vendor
        with:
          path: |
            elwood-term/wezterm/vendor
            elwood-term/wezterm/.cargo/config
          key: vendor-${{ runner.os }}-${{ hashFiles('elwood-term/wezterm/Cargo.lock') }}

      - name: Vendor Dependencies
        if: steps.cache-vendor.outputs.cache-hit != 'true'
        run: cargo vendor --locked --versioned-dirs >> .cargo/config

      - name: Install System Deps (Linux)
        if: runner.os == 'Linux'
        run: |
          apt-get update
          env CI=yes ./get-deps

      - name: Install macOS Targets
        if: runner.os == 'macOS'
        run: |
          rustup target add aarch64-apple-darwin
          rustup target add x86_64-apple-darwin

      # --- macOS: dual-arch build ---
      - name: Build (macOS Intel)
        if: runner.os == 'macOS'
        run: cargo build --target x86_64-apple-darwin -p wezterm-gui --release --features elwood

      - name: Build (macOS ARM)
        if: runner.os == 'macOS'
        run: cargo build --target aarch64-apple-darwin -p wezterm-gui --release --features elwood

      # --- Linux / Windows: single-arch build ---
      - name: Build (Linux/Windows)
        if: runner.os != 'macOS'
        run: cargo build -p wezterm-gui --release --features elwood
        shell: bash

      # --- Tests ---
      - uses: baptiste0928/cargo-install@v3
        with:
          crate: cargo-nextest
          cache-key: ${{ runner.os }}

      - name: Test
        run: cargo nextest run --all --no-fail-fast
        shell: bash

      # --- Package ---
      - name: Package
        run: bash ci/deploy.sh
        shell: bash

      - uses: actions/upload-artifact@v4
        with:
          name: elwood-terminal-${{ matrix.name }}
          path: |
            Elwood-Terminal-*.zip
            Elwood-Terminal-*.exe
            elwood-terminal-*.deb
            elwood-terminal-*.tar.xz
```

### 6.2 Release Workflow

```yaml
# .github/workflows/elwood-term-release.yml
name: Elwood Terminal Release

on:
  push:
    tags:
      - "elwood-term-v*"

jobs:
  build-macos:
    runs-on: macos-latest
    env:
      CARGO_INCREMENTAL: "0"
      SCCACHE_GHA_ENABLED: "true"
      RUSTC_WRAPPER: "sccache"
      MACOSX_DEPLOYMENT_TARGET: "10.15"
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive
      - uses: dtolnay/rust-toolchain@stable
      - uses: mozilla-actions/sccache-action@v0.0.9
      - name: Add targets
        run: |
          rustup target add aarch64-apple-darwin
          rustup target add x86_64-apple-darwin
      - name: Build Intel
        working-directory: elwood-term/wezterm
        run: |
          cargo build --target x86_64-apple-darwin -p wezterm-gui --release --features elwood
          cargo build --target x86_64-apple-darwin -p wezterm-mux-server --release
      - name: Build ARM
        working-directory: elwood-term/wezterm
        run: |
          cargo build --target aarch64-apple-darwin -p wezterm-gui --release --features elwood
          cargo build --target aarch64-apple-darwin -p wezterm-mux-server --release
      - name: Package & Sign
        working-directory: elwood-term/wezterm
        env:
          MACOS_APPLEID: ${{ secrets.MACOS_APPLEID }}
          MACOS_APP_PW: ${{ secrets.MACOS_APP_PW }}
          MACOS_CERT: ${{ secrets.MACOS_CERT }}
          MACOS_CERT_PW: ${{ secrets.MACOS_CERT_PW }}
          MACOS_TEAM_ID: ${{ secrets.MACOS_TEAM_ID }}
        run: bash ci/deploy.sh
      - uses: actions/upload-artifact@v4
        with:
          name: macos
          path: elwood-term/wezterm/Elwood-Terminal-*.zip

  build-linux:
    runs-on: ubuntu-latest
    container: ubuntu:24.04
    steps:
      - name: Setup
        run: |
          apt-get update
          apt-get install -y git curl
          git config --global --add safe.directory /__w/elwood-pro/elwood-pro
      - uses: actions/checkout@v4
        with:
          submodules: recursive
      - uses: dtolnay/rust-toolchain@stable
      - uses: mozilla-actions/sccache-action@v0.0.9
      - name: Install Deps
        working-directory: elwood-term/wezterm
        run: env CI=yes ./get-deps
      - name: Build
        working-directory: elwood-term/wezterm
        env:
          CARGO_INCREMENTAL: "0"
          SCCACHE_GHA_ENABLED: "true"
          RUSTC_WRAPPER: "sccache"
        run: |
          cargo build -p wezterm-gui --release --features elwood
          cargo build -p wezterm-mux-server --release
          cargo build -p strip-ansi-escapes --release
      - name: Package
        working-directory: elwood-term/wezterm
        run: bash ci/deploy.sh
      - uses: actions/upload-artifact@v4
        with:
          name: linux
          path: |
            elwood-term/wezterm/elwood-terminal-*.deb
            elwood-term/wezterm/elwood-terminal-*.tar.xz

  build-windows:
    runs-on: windows-2022
    env:
      CARGO_INCREMENTAL: "0"
      SCCACHE_GHA_ENABLED: "true"
      RUSTC_WRAPPER: "sccache"
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive
      - uses: dtolnay/rust-toolchain@stable
        with:
          target: x86_64-pc-windows-msvc
      - uses: mozilla-actions/sccache-action@v0.0.9
      - name: Build
        working-directory: elwood-term/wezterm
        shell: cmd
        run: |
          PATH C:\Strawberry\perl\bin;%PATH%
          cargo build -p wezterm-gui --release --features elwood
          cargo build -p wezterm-mux-server --release
          cargo build -p strip-ansi-escapes --release
      - name: Package
        working-directory: elwood-term/wezterm
        shell: bash
        run: bash ci/deploy.sh
      - uses: actions/upload-artifact@v4
        with:
          name: windows
          path: |
            elwood-term/wezterm/Elwood-Terminal-*.zip
            elwood-term/wezterm/Elwood-Terminal-*.exe

  publish:
    needs: [build-macos, build-linux, build-windows]
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      - uses: actions/download-artifact@v4
      - name: Checksums
        run: |
          for f in macos/* linux/* windows/*; do
            sha256sum "$f" > "$f.sha256"
          done
      - name: Create Release
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        run: |
          TAG=${GITHUB_REF#refs/tags/}
          gh release create "$TAG" \
            --title "Elwood Terminal ${TAG#elwood-term-v}" \
            --generate-notes \
            macos/* linux/* windows/*
```

### 6.3 Build Time Estimates

Based on WezTerm's build characteristics (large workspace, vendored deps, GPU crates):

| Platform | Cold Build | Cached Build (sccache) | Notes |
|----------|------------|------------------------|-------|
| macOS (2 arches) | ~25-35 min | ~8-12 min | Dual-target builds are sequential |
| Linux (Ubuntu) | ~15-20 min | ~5-8 min | Container builds have no OS cache |
| Windows | ~20-30 min | ~8-12 min | Perl + MSVC toolchain overhead |

Key optimizations:
- `sccache` with GHA backend reduces rebuild times by 60-70%
- `cargo vendor` caching avoids network-dependent crate downloads
- `cargo-nextest` parallelizes tests better than `cargo test`
- Consider splitting build and test into separate jobs for faster failure feedback

### 6.4 Caching Strategy

```yaml
# 1. sccache (compilation cache, GHA backend)
env:
  SCCACHE_GHA_ENABLED: "true"
  RUSTC_WRAPPER: "sccache"

# 2. Vendor cache (dependency sources)
- uses: actions/cache@v4
  with:
    path: |
      elwood-term/wezterm/vendor
      elwood-term/wezterm/.cargo/config
    key: vendor-${{ runner.os }}-${{ hashFiles('elwood-term/wezterm/Cargo.lock') }}

# 3. cargo-nextest binary cache
- uses: baptiste0928/cargo-install@v3
  with:
    crate: cargo-nextest
    cache-key: ${{ runner.os }}
```

---

## 7. Implementation Plan

### Phase 1: Branding Foundation (Priority: HIGH)

**Goal**: Complete the wezterm-to-elwood rename for all user-facing surfaces.

| Step | Task | Files | Effort |
|------|------|-------|--------|
| 1.1 | Create Elwood icon set (.icns, .ico, .png, .svg) | `assets/icon/` | Design task |
| 1.2 | Rename macOS bundle `WezTerm.app` -> `Elwood Terminal.app` | `assets/macos/` | 1hr |
| 1.3 | Update Info.plist (bundle ID, names, icons, usage strings) | `assets/macos/.../Info.plist` | 1hr |
| 1.4 | Update `.desktop` file | `assets/wezterm.desktop` -> `assets/elwood.desktop` | 30min |
| 1.5 | Update AppData XML | `assets/wezterm.appdata.xml` -> `assets/elwood.appdata.xml` | 30min |
| 1.6 | Update Windows installer | `ci/windows-installer.iss` | 1hr |
| 1.7 | Update Windows build.rs resource strings | `wezterm-gui/build.rs` | 30min |
| 1.8 | Rename env vars with backward-compat fallbacks | `env-bootstrap/src/lib.rs`, config | 2hr |
| 1.9 | Update shell integration script | `assets/shell-integration/wezterm.sh` | 1hr |
| 1.10 | Update shell completions | `assets/shell-completion/*` | 1hr |
| 1.11 | Update version system (`WEZTERM_CI_TAG` -> `ELWOOD_CI_TAG`) | `wezterm-version/` | 30min |
| 1.12 | Rename data directories with migration | `config/src/config.rs` | 1hr |

### Phase 2: Build Infrastructure (Priority: HIGH)

| Step | Task | Effort |
|------|------|--------|
| 2.1 | Create `ci/deploy-elwood.sh` (adapted from `ci/deploy.sh`) | 3hr |
| 2.2 | Create `.github/workflows/elwood-term-ci.yml` (PR builds) | 2hr |
| 2.3 | Create `.github/workflows/elwood-term-release.yml` (tag builds) | 2hr |
| 2.4 | Test macOS build locally (ad-hoc signing) | 1hr |
| 2.5 | Test Linux build in container | 1hr |
| 2.6 | Test Windows build | 1hr |

### Phase 3: Distribution Channels (Priority: MEDIUM)

| Step | Task | Effort |
|------|------|--------|
| 3.1 | Create Homebrew cask template | 1hr |
| 3.2 | Create Homebrew tap repository | 30min |
| 3.3 | Add DMG creation to macOS build | 1hr |
| 3.4 | Create AppImage build script | 2hr |
| 3.5 | Create AUR PKGBUILD | 1hr |
| 3.6 | Create winget manifest | 30min |
| 3.7 | Create Scoop manifest | 30min |
| 3.8 | Update Flatpak manifest | 1hr |

### Phase 4: Polish (Priority: LOW)

| Step | Task | Effort |
|------|------|--------|
| 4.1 | Automated version bumping (semver tags) | 1hr |
| 4.2 | Release notes generation from commits | 1hr |
| 4.3 | Update check mechanism (optional, built into GUI) | 3hr |
| 4.4 | Code signing for Windows (requires certificate) | 2hr |
| 4.5 | macOS notarization (requires Apple Developer account) | 2hr |

### Total Estimated Effort

- **Phase 1**: ~10 hours (mechanical rename, highest priority)
- **Phase 2**: ~10 hours (CI/CD infrastructure)
- **Phase 3**: ~7.5 hours (distribution channels)
- **Phase 4**: ~9 hours (polish, can defer)

---

## Appendix A: Key File Inventory

Files requiring changes (sorted by priority):

```
# macOS
assets/macos/WezTerm.app/Contents/Info.plist
assets/macos/WezTerm.app/Contents/Resources/terminal.icns

# Linux
assets/wezterm.desktop
assets/wezterm.appdata.xml
assets/wezterm-nautilus.py
assets/icon/terminal.png

# Windows
assets/windows/terminal.ico
ci/windows-installer.iss

# Build system
wezterm-gui/build.rs
wezterm-version/build.rs
wezterm-version/src/lib.rs
ci/deploy.sh

# Runtime
env-bootstrap/src/lib.rs
config/src/lib.rs
config/src/config.rs
config/src/lua.rs
mux/src/domain.rs
wezterm-client/src/client.rs
wezterm-mux-server/src/main.rs
wezterm-mux-server-impl/src/sessionhandler.rs
wezterm-gui/src/main.rs

# Shell integration
assets/shell-integration/wezterm.sh
assets/shell-completion/bash
assets/shell-completion/zsh
assets/shell-completion/fish

# Packaging templates
ci/wezterm-homebrew-macos.rb.template
assets/flatpak/org.wezfurlong.wezterm.template.json
assets/flatpak/org.wezfurlong.wezterm.json
```

## Appendix B: Secrets Required

| Secret | Platform | Purpose |
|--------|----------|---------|
| `MACOS_TEAM_ID` | macOS | Developer ID for code signing |
| `MACOS_APPLEID` | macOS | Apple ID for notarization |
| `MACOS_APP_PW` | macOS | App-specific password for notarization |
| `MACOS_CERT` | macOS | Base64-encoded .p12 certificate |
| `MACOS_CERT_PW` | macOS | Certificate password (base64) |
| `WINDOWS_CERT` | Windows | Code signing certificate (optional) |
| `WINDOWS_CERT_PW` | Windows | Certificate password (optional) |
| `GH_PAT` | All | Personal access token for Homebrew tap push |

## Appendix C: Identifier Summary

| Context | Identifier |
|---------|-----------|
| macOS Bundle ID | `io.elwood.terminal` |
| Linux Desktop ID | `io.elwood.terminal` |
| Windows AppUserModelID | `io.elwood.terminal` |
| Flatpak App ID | `io.elwood.terminal` |
| Homebrew Cask token | `elwood-terminal` |
| winget Package ID | `ElwoodProject.ElwoodTerminal` |
| Scoop manifest | `elwood-terminal` |
| AUR package | `elwood-terminal-bin` |
| Config directory | `~/.elwood/` |
| XDG config | `$XDG_CONFIG_HOME/elwood/` |
| Data directory | `~/.local/share/elwood/` |
| Unix socket | `/tmp/elwood-mux-$USER` |
