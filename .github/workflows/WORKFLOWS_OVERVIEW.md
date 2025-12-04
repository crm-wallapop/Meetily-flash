# GitHub Actions Workflows Overview

This document provides a quick overview of all available CI/CD workflows in this repository.

## Workflow Files

### 1. **build-devtest.yml** - DevTest Branch CI/CD ⚡
**Purpose:** Fast builds for development and testing on `devtest` branch

**Key Features:**
- ⊘ **Signing OFF by default** (faster builds)
- ✅ **Optional signing** via PR message `[sign]`
- 🤖 Auto-comments on PRs with status
- 🚀 All platforms in parallel
- 📦 14-day artifact retention

**Triggers:**
- Pull requests to `devtest`
- Direct pushes to `devtest`
- Manual dispatch

**Use When:**
- Regular development work
- Testing features before merging
- Need fast feedback

**Enable Signing:**
Add `[sign]` to PR description or commit message

---

### 2. **build-macos.yml** - macOS Standalone Builds 🍎
**Purpose:** Build and test specifically for Apple Silicon (M1/M2/M3)

**Key Features:**
- ✅ Apple Developer Certificate signing
- ✅ Notarization with Apple ID
- ✅ Complete test suite (lint, type-check, clippy, tests)
- ✅ Signature verification
- 🎯 macOS-focused optimizations

**Triggers:**
- Manual dispatch (with signing option)
- Pull requests to `main`/`develop`
- Pushes to `main`/`develop`

**Use When:**
- macOS-specific development
- Testing Metal GPU acceleration
- Verifying macOS-specific features

**Outputs:**
- `.dmg` installer
- `.app` bundle

---

### 3. **build-windows.yml** - Windows Standalone Builds 🪟
**Purpose:** Build and test specifically for Windows x64

**Key Features:**
- ✅ DigiCert KeyLocker signing (cloud HSM)
- ✅ Signs both MSI and NSIS installers
- ✅ Complete test suite
- ✅ Signature verification with PowerShell
- ✅ MSI installer validation

**Triggers:**
- Manual dispatch (with signing option)
- Pull requests to `main`/`develop`
- Pushes to `main`/`develop`

**Use When:**
- Windows-specific development
- Testing CUDA/Vulkan GPU acceleration
- Verifying Windows-specific features

**Outputs:**
- `.msi` installer
- `.exe` NSIS installer

---

### 4. **build-linux.yml** - Linux Standalone Builds 🐧
**Purpose:** Build and test for Linux distributions

**Key Features:**
- ✅ Support for Ubuntu 22.04 and 24.04
- ✅ Multiple bundle formats (DEB, AppImage, RPM)
- ✅ Tauri updater signing
- ✅ AppImage compatibility fixes
- ✅ Package verification

**Triggers:**
- Manual dispatch (with Ubuntu version and bundle type options)
- Pull requests to `main`/`develop`
- Pushes to `main`/`develop`

**Use When:**
- Linux-specific development
- Testing Vulkan GPU acceleration
- Verifying package formats

**Outputs:**
- `.deb` package (Ubuntu/Debian)
- `.AppImage` portable
- `.rpm` package (Fedora/RHEL)

---

### 5. **build-test.yml** - Multi-Platform Test Builds 🧪
**Purpose:** Test builds across all platforms with signing

**Key Features:**
- ✅ **Signing ON by default**
- ✅ All platforms in parallel
- ✅ Uses reusable `build.yml` workflow
- 📦 30-day artifact retention
- 🏷️ Artifacts prefixed with `meetily-test-`

**Triggers:**
- Manual dispatch only

**Use When:**
- Pre-release testing
- Verifying signing infrastructure
- Testing across all platforms simultaneously

---

### 6. **build.yml** - Reusable Build Workflow 🔧
**Purpose:** Shared workflow used by other workflows

**Key Features:**
- ⚙️ Reusable workflow (called by others)
- 🎛️ Highly configurable inputs
- 🔄 Used by `build-test.yml` and `release.yml`

**Not directly triggered** - used as a building block

---

### 7. **release.yml** - Production Release 🚀
**Purpose:** Create official releases with signed binaries

**Key Features:**
- ✅ **Signing REQUIRED**
- 📝 Creates GitHub Release (draft)
- 🏷️ Version tags from `tauri.conf.json`
- 📦 Uploads release assets
- 🎯 All platforms with optimized bundles

**Triggers:**
- Manual dispatch only

**Use When:**
- Ready to publish a new version
- Creating official release artifacts

**Outputs:**
- GitHub Release (draft)
- All platform installers
- Release notes auto-generated

---

## Quick Decision Guide

### "I'm developing a new feature..."
→ **Use `build-devtest.yml`** (push to `devtest` or create PR)
- Fast builds, no signing
- Add `[sign]` if you need signed builds for testing

### "I need to test macOS-specific code..."
→ **Use `build-macos.yml`** (manual dispatch)
- Focus on macOS
- Optional signing

### "I need to test Windows-specific code..."
→ **Use `build-windows.yml`** (manual dispatch)
- Focus on Windows
- Optional signing

### "I need to test Linux packages..."
→ **Use `build-linux.yml`** (manual dispatch)
- Choose Ubuntu version
- Choose bundle types

### "I need signed builds for all platforms..."
→ **Use `build-test.yml`** (manual dispatch)
- All platforms
- Signing enabled
- Full verification

### "I'm ready to release..."
→ **Use `release.yml`** (manual dispatch)
- Creates GitHub Release
- All platforms, fully signed
- Production-ready artifacts

---

## Comparison Matrix

| Workflow | Platforms | Default Signing | Speed | Retention | Use Case |
|----------|-----------|----------------|-------|-----------|----------|
| `build-devtest.yml` | All | ⊘ OFF | ⚡ Fast | 14 days | Development |
| `build-macos.yml` | macOS | Optional | Medium | 30 days | macOS dev |
| `build-windows.yml` | Windows | Optional | Medium | 30 days | Windows dev |
| `build-linux.yml` | Linux | Optional | Medium | 30 days | Linux dev |
| `build-test.yml` | All | ✅ ON | Slow | 30 days | Pre-release |
| `release.yml` | All | ✅ REQUIRED | Slow | Permanent | Release |

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

## Common Tasks

### Run unsigned build for quick testing
1. Push to `devtest` branch or create PR to `devtest`
2. Wait ~25-30 minutes
3. Download artifacts

### Run signed build for production testing
1. Create PR to `devtest` with `[sign]` in description
2. Wait ~35-45 minutes
3. Download signed artifacts

### Test specific platform
1. Go to Actions → Select platform workflow
2. Run workflow with desired options
3. Download artifacts

### Create release
1. Update version in `tauri.conf.json`, `package.json`, `Cargo.toml`
2. Commit and push to main
3. Go to Actions → Release
4. Run workflow
5. Review draft release on GitHub
6. Publish when ready

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

---

## Performance Tips

1. **Use devtest workflow** for regular development (fastest)
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

### Workflow not triggering
- Check branch name matches trigger configuration
- Verify file paths in PR match trigger paths
- Manual workflows require explicit run

---

## Support

For issues with workflows:
1. Check workflow logs in Actions tab
2. Review this documentation
3. Check `README_DEVTEST.md` for devtest-specific help
4. Contact DevOps team
