# GitHub Actions Workflows Overview

This document provides a quick overview of all available CI/CD workflows in this repository.

**Note:** Most build/release workflows run manually (`workflow_dispatch`). Four run automatically: `pr-build.yml` (pull requests and pushes to `main`), `opencode-review.yml` (pull-request events), `issue-enforce.yml` (issue events), and `issue-grace.yml` (`/reopen` comments and a nightly check of all open issues).

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

### 9. **issue-enforce.yml** - Issue Template Checks

**Purpose:** Check open issues against the same required fields as the issue forms.

**Key Features:**
- Applies the same template rules to every author and actor, including maintainers
- Recognizes the `[BUG]`, `[FEATURE]`, `[DOCS]`, and `[QUESTION]` title prefixes
- Requires information in each required section and checks each required box
- Keeps deeper Markdown headings inside their field and ignores code samples when checking required boxes
- Comments with the current failures, adds `invalid-template`, and closes incomplete issues
- Allows unlimited edits while `template-grace` is present, even after an edit completes the template
- Keeps grace until the nightly check; removing the label starts a normal template check
- Explains label changes when an issue follows its template
- Does no work on closed issues
- Checks the current state, title, body, and labels before each automatic change; a changed issue stops that pass
- Stopped passes leave the issue for its next event or nightly check; GitHub API calls are separate operations
- Shares a queue with `/reopen` jobs; waiting jobs remain queued instead of replacing earlier requests
- Uses `.github/scripts/issue-workflows.js` with the nightly workflow, so both use the same checks and closure explanations

**Triggers:**
- Automatically on open issue events (`opened`, `reopened`, `edited`)
- Automatically when someone removes `template-grace` from an open issue

**Use When:**
- Runs automatically; no manual dispatch is needed

---

### 10. **issue-grace.yml** - Reopen and Nightly Checks

**Purpose:** Give any user one reopen per issue and let maintainers grant more grace periods.

**Key Features:**
- Anyone can comment `/reopen` on a closed issue with `invalid-template`
- The first successful command adds `template-grace` and `invalid-template-again`, then reopens the issue
- `template-grace` permits unlimited edits until the next nightly check at 04:00 UTC
- `invalid-template-again` means that later `/reopen` commands need a maintainer; there is no reopen counter
- Maintainers with triage, write, maintain, or admin access can use `/reopen` without a limit
- Checks the command author's current permissions when a later reopen needs a maintainer
- Checks no title or body while an issue is closed; only an explicit `/reopen` request can process a closed issue
- A failed command rolls back new request labels and keeps an existing maintainer-only label; the bot reports any rollback failure
- A failed API response triggers a state check before rollback; an open issue keeps grace and the maintainer-only marker
- If the state check also fails, the bot keeps the labels and explains the uncertainty
- Each command response explains the action or refusal
- The nightly check covers **all open issues**, including older issues without a grace label
- Complete issues stay open; the bot removes `template-grace` and `invalid-template`, but keeps `invalid-template-again`
- Incomplete issues receive a comment with the current failures and close; the bot removes grace before closure
- Closed issues have no scheduled checks or automatic label cleanup
- A manual closure leaves the labels alone; closed issues remain outside the nightly check
- Retained grace after a manual close and reopen remains allowed intentionally
- Comments without `/reopen` skip the command job; the script accepts only the command itself, ignoring case and surrounding whitespace
- Uses `queue: max` with cancellation disabled; GitHub permits up to 100 waiting jobs in each queue and cancels new arrivals beyond that limit
- Tests run in `pr-build.yml`; run them locally with `node --test .github/scripts/issue-workflows.test.cjs`
- `actionlint` 1.7.12 does not recognize GitHub's `queue` field. Use its exact compatibility filter: `-ignore 'unexpected key "queue" for "concurrency" section'`. Queue settings also have direct tests.

**Triggers:**
- Automatically on issue comments (`/reopen`)
- Nightly at 04:00 UTC
- Manual dispatch (`workflow_dispatch`) for the nightly checks, with an optional dry run

**Use When:**
- Runs automatically; use manual dispatch to preview or start the nightly checks

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
