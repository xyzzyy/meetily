# GitHub Actions Workflows Overview

This document provides a quick overview of all available CI/CD workflows in this repository.

**Note:** Most build/release workflows run manually (`workflow_dispatch`). Four run automatically: `pr-build.yml` (pull requests and pushes to `main`), `opencode-review.yml` (pull-request events), `issue-enforce.yml` (issue events), and `issue-grace.yml` (`/reopen` comments and a nightly expiry sweep).

## Workflow Files

### 1. **build-devtest.yml** - DevTest Builds
**Purpose:** Fast builds for development and testing

**Key Features:**
- Signing OFF by default (faster builds)
- Optional signing via workflow dispatch input
- All platforms in parallel
- 14-day artifact retention

**Triggers:**
- Manual dispatch only

**Use When:**
- Regular development work
- Testing features
- Need fast feedback

---

### 2. **build-macos.yml** - macOS Standalone Builds
**Purpose:** Build and test specifically for Apple Silicon (M1/M2/M3)

**Key Features:**
- Apple Developer Certificate signing (optional)
- Notarization with Apple ID
- Signature verification
- macOS-focused optimizations

**Triggers:**
- Manual dispatch only

**Use When:**
- macOS-specific development
- Testing Metal GPU acceleration
- Verifying macOS-specific features

**Outputs:**
- `.dmg` installer
- `.app` bundle

---

### 3. **build-windows.yml** - Windows Standalone Builds
**Purpose:** Build and test specifically for Windows x64

**Key Features:**
- DigiCert KeyLocker signing (cloud HSM)
- Signs both MSI and NSIS installers
- Signature verification with PowerShell
- MSI installer validation

**Triggers:**
- Manual dispatch only

**Use When:**
- Windows-specific development
- Testing Vulkan GPU acceleration
- Verifying Windows-specific features

**Outputs:**
- `.msi` installer
- `.exe` NSIS installer

---

### 4. **build-linux.yml** - Linux Standalone Builds
**Purpose:** Build and test for Linux distributions

**Key Features:**
- Support for Ubuntu 22.04 and 24.04
- Multiple bundle formats (DEB, AppImage, RPM)
- Tauri updater signing
- AppImage compatibility fixes
- Package verification

**Triggers:**
- Manual dispatch only

**Use When:**
- Linux-specific development
- Testing Vulkan GPU acceleration
- Verifying package formats

**Outputs:**
- `.deb` package (Ubuntu/Debian)
- `.AppImage` portable
- `.rpm` package (Fedora/RHEL)

---

### 5. **build-test.yml** - Multi-Platform Test Builds
**Purpose:** Test builds across all platforms with signing

**Key Features:**
- Signing ON by default
- All platforms in parallel
- Uses reusable `build.yml` workflow
- 30-day artifact retention
- Artifacts prefixed with `meetily-test-`

**Triggers:**
- Manual dispatch only

**Use When:**
- Pre-release testing
- Verifying signing infrastructure
- Testing across all platforms simultaneously

---

### 6. **build.yml** - Reusable Build Workflow
**Purpose:** Shared workflow used by other workflows

**Key Features:**
- Reusable workflow (called by others)
- Highly configurable inputs
- Used by `build-test.yml` and `release.yml`

**Not directly triggered** - used as a building block

---

### 7. **release.yml** - Production Release
**Purpose:** Create official releases with signed binaries

**Key Features:**
- Signing REQUIRED
- Creates GitHub Release (draft)
- Version tags from `tauri.conf.json`
- Uploads release assets
- **macOS and Windows only** (Linux excluded from production releases)
- Auto-generates `latest.json` for Tauri updater
- **Auto-increment versioning**: If tag exists, auto-increments (e.g., `0.1.1` -> `0.1.1.1` -> `0.1.1.2`, up to `.100`)

**Triggers:**
- Manual dispatch only

**Use When:**
- Ready to publish a new version
- Creating official release artifacts

**Outputs:**
- GitHub Release (draft)
- macOS: DMG installer, app.tar.gz (updater), .sig
- Windows: MSI installer (signed), NSIS installer (signed), .sig files
- Updater manifest: latest.json
- Release notes auto-generated

**Version Behavior:**
- If `v0.1.1` tag doesn't exist: creates `v0.1.1`
- If `v0.1.1` exists: creates `v0.1.1.1`
- If `v0.1.1.1` exists: creates `v0.1.1.2`
- Maximum: `v0.1.1.100` (then update `tauri.conf.json`)

**Note:** Linux builds are not included in releases. Use `build-linux.yml` for Linux testing.

---

### 8. **pr-main-check.yml** - Validation Check
**Purpose:** Quick validation of version and configuration

**Key Features:**
- No builds triggered
- Validates version format
- Shows current branch info
- Provides next steps guidance

**Triggers:**
- Manual dispatch only

**Use When:**
- Quick configuration check
- Before running full builds

---

### 9. **issue-enforce.yml** - Issue Template Enforcement
**Purpose:** Enforce the issue templates for issues created outside the web UI

**Key Features:**
- Second enforcement layer on top of the YAML issue forms in `.github/ISSUE_TEMPLATE/`
- Shared section/checkbox validation lives in `.github/scripts/issue-template.js` (used by both issue workflows)
- Recognizes the `[BUG]`/`[FEATURE]`/`[DOCS]`/`[QUESTION]` title prefixes
- Auto-applies the matching label (bug/enhancement/documentation/question)
- Warns, labels `invalid-template`, and auto-closes non-conforming issues (warn + auto-close)
- Grandfathers existing issues: nothing is backfilled, and an open issue is only re-evaluated after it is edited or reopened
- Does not reopen closed issues itself; the author's one-time `/reopen` command is handled by `issue-grace.yml`
- Never closes an issue that is inside its `template-grace` window; the nightly sweep owns expiry
- Maintainer-authored issues and maintainer-triggered edits/reopens are exempt

**Triggers:**
- Automatically on issue events (`opened`, `reopened`, `edited`)

**Use When:**
- Runs by itself; no manual dispatch needed

---

### 10. **issue-grace.yml** - One-Time Reopen and Nightly Expiry
**Purpose:** Give authors one automatic way back into a template-closed issue, then expire that window nightly.

**Key Features:**
- Handles `/reopen` comments from the issue author on closed issues labeled `invalid-template`
- Re-validates immediately: fixed issues reopen as normal; still-incomplete issues reopen with `template-grace` (the active window) and `template-grace-used` (the one-time chance has been consumed)
- Only the author can use the command, and only once per issue; later attempts get a one-time pointer to the maintainers
- Nightly sweep at 04:00 UTC re-validates open `template-grace` issues: fixed ones pass, incomplete ones are closed again and the active label is removed
- Removes `template-grace` if the issue is closed during the window, so a later maintainer reopen is not affected by a stale label
- Maintainers can always reopen manually; maintainer-triggered events are exempt from `issue-enforce.yml`

**Triggers:**
- Automatically on issue comments (`/reopen`)
- Automatically on issue close (stale-window cleanup)
- Nightly at 04:00 UTC
- Manual dispatch (`workflow_dispatch`) for the expiry sweep, with an optional dry run

**Use When:**
- Runs by itself; the manual dispatch is useful to preview or force an expiry sweep

---

## How to Run Workflows

1. **Go to Actions tab** in GitHub repository
2. **Select workflow** from left sidebar
3. **Click "Run workflow"** button
4. **Select branch** to run against
5. **Configure options** (build type, signing, etc.)
6. **Click "Run workflow"** to start
7. **Monitor progress** in the Actions tab

---

## Quick Decision Guide

### "I'm developing a new feature..."
- **Use `build-devtest.yml`** (manual dispatch)
- Fast builds, no signing by default
- Enable signing checkbox if needed

### "I need to test macOS-specific code..."
- **Use `build-macos.yml`** (manual dispatch)
- Focus on macOS
- Optional signing

### "I need to test Windows-specific code..."
- **Use `build-windows.yml`** (manual dispatch)
- Focus on Windows
- Optional signing

### "I need to test Linux packages..."
- **Use `build-linux.yml`** (manual dispatch)
- Choose Ubuntu version
- Choose bundle types

### "I need signed builds for all platforms..."
- **Use `build-test.yml`** (manual dispatch)
- All platforms
- Signing enabled
- Full verification

### "I'm ready to release..."
- **Use `release.yml`** (manual dispatch)
- Creates GitHub Release
- All platforms, fully signed
- Production-ready artifacts

---

## Workflow Dependencies

```
build.yml (reusable)
    |-- build-test.yml (calls build.yml)
    |-- release.yml (calls build.yml)

Standalone (don't use build.yml):
    |-- build-macos.yml
    |-- build-windows.yml
    |-- build-linux.yml
    |-- build-devtest.yml
    |-- pr-main-check.yml (validation only)
```

---

## Comparison Matrix

| Workflow | Platforms | Default Signing | Speed | Retention | Use Case |
|----------|-----------|----------------|-------|-----------|----------|
| `build-devtest.yml` | All | OFF | Fast | 14 days | Development |
| `build-macos.yml` | macOS | Optional | Medium | 30 days | macOS dev |
| `build-windows.yml` | Windows | Optional | Medium | 30 days | Windows dev |
| `build-linux.yml` | Linux | Optional | Medium | 30 days | Linux dev |
| `build-test.yml` | All | ON | Slow | 30 days | Pre-release |
| `release.yml` | macOS + Windows | REQUIRED | Slow | Permanent | Release |

---

## Artifact Naming Convention

```
meetily-{workflow}-{platform}-{target}-{version}
```

**Examples:**
- `meetily-devtest-macOS-aarch64-apple-darwin-0.1.3`
- `meetily-test-windows-x86_64-pc-windows-msvc-0.1.3`
- `meetily-macos-aarch64-release-0.1.3`

---

## Required Secrets

All workflows require these secrets to be configured:

### macOS Signing
- `APPLE_CERTIFICATE` - Developer ID certificate (base64)
- `APPLE_CERTIFICATE_PASSWORD` - Certificate password
- `APPLE_ID` - Apple ID email
- `APPLE_PASSWORD` - App-specific password
- `APPLE_TEAM_ID` - Team ID
- `KEYCHAIN_PASSWORD` - Temporary keychain password

### Windows Signing (DigiCert)
- `SM_HOST` - DigiCert host URL
- `SM_API_KEY` - API key
- `SM_CLIENT_CERT_FILE_B64` - Client cert (base64)
- `SM_CLIENT_CERT_PASSWORD` - Client cert password
- `SM_CODE_SIGNING_CERT_SHA1_HASH` - Certificate hash

### Tauri Updater (All Platforms)
- `TAURI_SIGNING_PRIVATE_KEY` - Ed25519 private key
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` - Key password

### Application Configuration
- `MEETILY_RSA_PUBLIC_KEY` - License validation public key
- `SUPABASE_URL` - Online license verification
- `SUPABASE_ANON_KEY` - Supabase anonymous key

---

## Performance Tips

1. **Use devtest workflow** for routine development (fastest)
2. **Enable signing** only when necessary (adds 10-15 minutes)
3. **Test specific platforms** when working on platform-specific code
4. **Run full builds** (`build-test.yml`) before releases
5. **Cache is enabled** - subsequent builds are faster

---

## Troubleshooting

### Build fails with version error (Windows MSI)
- Ensure version in `tauri.conf.json` doesn't contain non-numeric pre-release identifiers
- Use `0.1.3` not `0.1.2-pro-trial`

### Signing fails
- Verify all required secrets are configured
- Check secret expiration dates
- Review workflow logs for specific errors

### Artifacts not available
- Check build succeeded completely
- Artifacts expire based on retention period
- Ensure `upload-artifacts` is enabled

### Workflow not appearing in Actions
- Verify YAML syntax is valid
- Check file is in `.github/workflows/` directory
- Ensure file extension is `.yml` or `.yaml`

---

## Support

For issues with workflows:
1. Check workflow logs in Actions tab
2. Review this documentation
3. Check `README_DEVTEST.md` for devtest-specific help
4. Check `ACCELERATION_GUIDE.md` for GPU/performance info
