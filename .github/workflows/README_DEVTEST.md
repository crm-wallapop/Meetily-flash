# DevTest Branch CI/CD Workflow

This document explains how to use the `build-devtest.yml` workflow for building and testing on the `devtest` branch.

## Overview

The DevTest workflow is specifically designed for development and testing purposes. It:
- Builds for all platforms (macOS, Windows, Linux)
- Has **code signing disabled by default** to speed up builds
- Allows **optional signing** via PR description or commit message
- Automatically comments on PRs with build status
- Uploads artifacts for testing

## Triggering the Workflow

The workflow runs automatically on:
1. **Pull Requests** targeting `devtest` branch
2. **Direct pushes** to `devtest` branch
3. **Manual dispatch** via GitHub Actions UI

## Enabling Code Signing

By default, **signing is OFF** for devtest builds. To enable signing:

### Option 1: PR Description (Recommended)

Add one of these keywords anywhere in your Pull Request description:

```
[sign]
[sign-build]
[with-signing]
sign: true
sign: yes
```

**Example PR Description:**
```markdown
## Changes
- Fixed audio mixing bug
- Updated UI components

[sign]

This PR needs signed builds for testing on production-like environments.
```

### Option 2: Commit Message

Include the keyword in your commit message when pushing directly to `devtest`:

```bash
git commit -m "fix: audio mixing improvements [sign]"
git push origin devtest
```

### Option 3: Manual Workflow Dispatch

1. Go to **Actions** → **Build and Test - DevTest**
2. Click **Run workflow**
3. Select the branch
4. Check **"Sign the build"** checkbox
5. Click **Run workflow**

## How It Works

### 1. Detection Phase

The workflow first runs a `detect-signing` job that:
- Checks if signing was requested via PR description, commit message, or manual input
- Posts a comment on the PR indicating whether signing is enabled
- Outputs the signing decision to subsequent jobs

### 2. Build Phase

Builds run in parallel for all platforms:
- **macOS** (Apple Silicon / aarch64)
- **Windows** (x64)
- **Linux Ubuntu 22.04** (DEB package)
- **Linux Ubuntu 24.04** (AppImage + RPM)

Each build includes:
- ✅ Frontend linting (`pnpm run lint`)
- ✅ Frontend type checking (`pnpm run type-check`)
- ✅ Rust unit tests (`cargo test`)
- ✅ Rust linting (`cargo clippy`)
- ✅ Platform-specific bundling
- ✅ **Optional**: Code signing (if enabled)
- ✅ Signature verification (if signed)
- ✅ Artifact uploads

### 3. Status Update Phase

After all builds complete:
- Posts a final comment on the PR with overall status
- Includes links to workflow logs
- Shows signing status

## Code Signing Details

When signing is enabled:

### macOS
- Uses **Apple Developer Certificate** from secrets
- Performs **notarization** with Apple ID
- Signs both DMG and .app bundle
- Verifies signatures with `codesign` and `spctl`

### Windows
- Uses **DigiCert KeyLocker** (cloud HSM)
- Signs both MSI and NSIS installers
- Verifies signatures with PowerShell

### Linux
- Uses **Tauri updater signing** (Ed25519)
- Signs update manifests for auto-updater

## Build Artifacts

Artifacts are automatically uploaded and retained for **14 days**:

- **macOS**: `*.dmg`, `*.app`
- **Windows**: `*.msi`, `*.exe`
- **Linux**: `*.deb`, `*.AppImage`, `*.rpm`

Download from:
- **Actions** → **Build and Test - DevTest** → Select workflow run → **Artifacts** section

## PR Comments

The workflow posts two comments on PRs:

### Initial Comment (Build Started)
```
## DevTest Build Status

✅ Code signing enabled for this build
OR
⊘ Code signing disabled (default for devtest)

To enable signing, add `[sign]` to your PR description.

Build started: 2025-01-15T10:30:00Z
Workflow run: [View logs](...)
```

### Final Comment (Build Complete)
```
## DevTest Build Complete ✅

Build Status: success
Signing Status: ✅ Signed
Completed: 2025-01-15T11:00:00Z

[View full workflow run](...)

---
💡 To enable code signing on devtest builds, add `[sign]` to your PR description.
```

## Examples

### Example 1: Unsigned Build (Default)

**PR Description:**
```markdown
## Bug Fix: Audio Pipeline Improvements

- Fixed microphone capture on Windows
- Improved VAD detection threshold
- Updated error handling
```

**Result:** Builds without signing (faster, ~25-30 minutes)

---

### Example 2: Signed Build

**PR Description:**
```markdown
## Bug Fix: Audio Pipeline Improvements

[sign]

- Fixed microphone capture on Windows
- Improved VAD detection threshold
- Updated error handling

Need signed builds to test on production-like environment.
```

**Result:** Builds with full code signing (~35-45 minutes)

---

### Example 3: Direct Push with Signing

```bash
git add .
git commit -m "fix: critical security update [sign-build]"
git push origin devtest
```

**Result:** Builds with signing enabled via commit message

## Troubleshooting

### Signing Not Detected

**Problem:** Added `[sign]` but builds are still unsigned

**Solutions:**
1. Ensure the keyword is in the **PR description** (not just a comment)
2. Check the initial workflow comment - it shows detection status
3. Try re-running the workflow after editing the PR description
4. Verify the keyword format matches one of: `[sign]`, `[sign-build]`, `[with-signing]`

### Build Failures

**Problem:** Build fails during signing phase

**Solutions:**
1. Check that all required secrets are configured:
   - `APPLE_CERTIFICATE`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`
   - `SM_HOST`, `SM_API_KEY`, `SM_CODE_SIGNING_CERT_SHA1_HASH`
   - `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
2. Review workflow logs for specific error messages
3. Try running without signing first to isolate the issue

### Artifacts Not Available

**Problem:** Can't download build artifacts

**Solutions:**
1. Check workflow status - artifacts only available after successful build
2. Artifacts expire after 14 days
3. Ensure `upload-artifacts` is enabled (default: true)

## Performance Comparison

| Build Type | Duration | When to Use |
|------------|----------|-------------|
| **Unsigned** (default) | ~25-30 min | Regular development, quick testing |
| **Signed** | ~35-45 min | Pre-release testing, production-like testing |

## Best Practices

1. **Use unsigned builds** for routine development and testing
2. **Enable signing** only when:
   - Testing production-like scenarios
   - Preparing for release
   - Testing installer behavior
   - Verifying code signing infrastructure

3. **Always test** linting and type checking locally before pushing

4. **Review** the initial PR comment to confirm signing status matches your intent

## Workflow Configuration

Located at: `.github/workflows/build-devtest.yml`

Key configuration:
- **Default signing:** OFF
- **Artifact retention:** 14 days
- **Parallel builds:** All platforms simultaneously
- **Auto-triggers:** PR, push, manual dispatch

## Related Workflows

- `build-macos.yml` - macOS-specific builds with signing
- `build-windows.yml` - Windows-specific builds with signing
- `build-linux.yml` - Linux-specific builds with signing
- `build-test.yml` - General test builds (all platforms with signing)
- `release.yml` - Production release workflow
