# Releasing & packaging

Phase 0 of the porting plan: get prebuilt binaries into testers' hands without
them needing a Rust toolchain.

## One-time setup (this isn't a git repo yet)

```sh
git init
git add -A
git commit -m "Initial commit"
gh repo create GW_TUI --source=. --public --push   # or set a remote manually
```

The CI (`.github/workflows/ci.yml`) then runs build+test on every push, and the
release workflow (`.github/workflows/release.yml`) fires on version tags.

## Cutting a release

1. Bump `version` in the root `Cargo.toml` (`[workspace.package]`) and refresh the
   lockfile: `cargo update -p gwm-core -p gwm-tui --precise <new-version>` — or
   just `cargo build` and commit the changed `Cargo.lock`.
2. Commit, then tag and push:

   ```sh
   git tag v0.2.0
   git push origin main --tags
   ```

3. `release.yml` builds these and attaches them (+ `.sha256`) to a GitHub Release
   named after the tag:
   - `x86_64-unknown-linux-gnu` — smallest, for recent distros
   - `x86_64-unknown-linux-musl` — fully static, runs on any distro (hand this to
     testers when unsure)
   - `aarch64-unknown-linux-gnu` — Raspberry Pi 4/5 etc.
   - **`lubeshop_<ver>_amd64.deb`** — Debian/Ubuntu package

   Each tarball contains the `lubeshop` binary + `README.md`. Testers just extract
   and run `./lubeshop`; the in-app **Tools** menu installs the external tools.

## Debian/Ubuntu package (.deb)

The `.deb` is configured in `crates/gwm-tui/Cargo.toml` under
`[package.metadata.deb]`. It installs `lubeshop` to `/usr/bin`, and marks the
Debian-packaged disk tools (`cpmtools`, `mtools`, `vice`) as **Recommends** so apt
offers them; the Python tools (gw, amitools) and unpackaged ones (AppleCommander,
HxC) are handled by the app's own distro-aware Tools menu.

Build it:

```sh
cargo install cargo-deb        # one-time
cargo deb -p gwm-tui           # → target/debian/lubeshop_<ver>_amd64.deb
```

Install / test it on Debian or Ubuntu:

```sh
sudo apt install ./lubeshop_0.1.0-1_amd64.deb   # pulls in the Recommends too
```

> **Build it on Debian/Ubuntu (or let CI do it).** On a non-Debian host (e.g. Arch)
> `dpkg-shlibdeps` isn't available, so the `Depends:` line comes out empty — the
> package still runs on a real Debian box (libc is always present) but won't
> declare its libc dependency. The release workflow builds the `.deb` on Ubuntu so
> that dependency is filled in correctly; prefer that artifact for distribution.

You can also trigger it manually from the Actions tab (`workflow_dispatch`) to
smoke-test the build before tagging.

## AUR package (Arch)

`packaging/PKGBUILD` builds `lubeshop` from the tagged source tarball. The wrapped
tools are `optdepends` (optional — the app installs them from its Tools menu), so
the base package is tiny.

To publish:

1. Edit `PKGBUILD`: set the `Maintainer` line and replace `GITHUB_USER` in `url`.
2. Push a matching `v$pkgver` tag (the `source=` URL points at it).
3. `updpkgsums` to fill the real `sha256sums` (or leave `SKIP`).
4. `makepkg --printsrcinfo > .SRCINFO`.
5. Test locally: `makepkg -si`.
6. Push `PKGBUILD` + `.SRCINFO` to an `aur/lubeshop` git remote.

Bump `pkgver` (and reset `pkgrel=1`) for each new upstream tag.

## Not yet (later phases)

Windows/macOS targets get added to the release matrix once the platform work
lands (see the porting-phase notes). Native Windows especially needs the tool
availability spikes done first.
