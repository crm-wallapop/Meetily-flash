#!/usr/bin/env node
/**
 * Auto-detect GPU and run Tauri with appropriate features
 */

const { execSync } = require('child_process');
const path = require('path');
const fs = require('fs');
const os = require('os');

// Get the command (dev or build)
const command = process.argv[2];
if (!command || !['dev', 'build'].includes(command)) {
  console.error('Usage: node tauri-auto.js [dev|build]');
  process.exit(1);
}

// Detect GPU feature
let feature = '';
try {
  const result = execSync('node scripts/auto-detect-gpu.js', {
    encoding: 'utf8',
    stdio: ['pipe', 'pipe', 'inherit']
  });
  feature = result.trim();
} catch (err) {
  // If detection fails, continue with no features
}

console.log(''); // Empty line for spacing

// Platform-specific environment variables
const platform = os.platform();
const env = { ...process.env };

if (platform === 'linux' && feature === 'cuda') {
    console.log('🐧 Linux/CUDA detected: Setting CMAKE flags for NVIDIA GPU');
    env.CMAKE_CUDA_ARCHITECTURES = '75';
    env.CMAKE_CUDA_STANDARD = '17';
    env.CMAKE_POSITION_INDEPENDENT_CODE = 'ON';
}

// Build llama-helper with matching GPU features
console.log('🦙 Building llama-helper sidecar...');
const helperDir = path.join(__dirname, '../../llama-helper');
// Note: llama-helper might not need the feature flags directly if it auto-detects, 
// but passing them ensures consistency if features are added later.
// For now, we build standard release as the helper uses runtime detection or compile-time features if configured.
// If llama-helper needs features, append them here. 
// Based on current Cargo.toml, it seems to rely on default or auto-detection, but we can pass features if needed.
// The user asked to ensure acceleration is used.
const helperFeatures = feature ? `--features ${feature}` : ''; 

// Use debug build for dev (faster iteration), release for production
const buildProfile = command === 'dev' ? '' : '--release';
const buildDir = command === 'dev' ? 'debug' : 'release';
const helperCmd = `cd ${helperDir} && cargo build ${buildProfile} ${helperFeatures}`;

console.log(`📦 Build profile: ${command === 'dev' ? 'debug (fast iteration)' : 'release (optimized)'}`);

try {
  execSync(helperCmd, { stdio: 'inherit', env });
  console.log('✅ llama-helper built successfully');
} catch (err) {
  console.error('❌ Failed to build llama-helper');
  process.exit(1);
}

// Detect target triple for proper sidecar naming
let targetTriple = '';
try {
  const rustcOutput = execSync('rustc -vV', { encoding: 'utf8' });
  const hostMatch = rustcOutput.match(/host:\s*(\S+)/);
  if (hostMatch) {
    targetTriple = hostMatch[1];
  }
} catch (err) {
  console.error('❌ Failed to detect Rust target triple');
  process.exit(1);
}

console.log(`🎯 Target triple: ${targetTriple}`);

// Copy binary to src-tauri/binaries/ with target triple suffix
const binariesDir = path.join(__dirname, '../src-tauri/binaries');
if (!fs.existsSync(binariesDir)) {
  fs.mkdirSync(binariesDir, { recursive: true });
}

const baseBinary = platform === 'win32' ? 'llama-helper.exe' : 'llama-helper';
const sidecarBinary = platform === 'win32'
  ? `llama-helper-${targetTriple}.exe`
  : `llama-helper-${targetTriple}`;

const srcPath = path.join(helperDir, `../target/${buildDir}`, baseBinary);
const destPath = path.join(binariesDir, sidecarBinary);

// Clean up old binaries to ensure fresh copy is used
try {
    const files = fs.readdirSync(binariesDir);
    for (const file of files) {
        if (file.startsWith('llama-helper')) {
            fs.unlinkSync(path.join(binariesDir, file));
        }
    }
    console.log('🧹 Cleaned up old binaries');
} catch (err) {
    console.warn('⚠️ Failed to clean binaries directory:', err.message);
}

if (fs.existsSync(srcPath)) {
  fs.copyFileSync(srcPath, destPath);
  // Ensure the binary is executable
  if (platform !== 'win32') {
    try {
      fs.chmodSync(destPath, 0o755);
      console.log('✅ Set executable permissions for llama-helper');
    } catch (err) {
      console.warn('⚠️ Failed to set executable permissions:', err.message);
    }
  }
  console.log(`✅ Copied llama-helper to ${destPath}`);
} else {
  console.error(`❌ llama-helper binary not found at ${srcPath}`);
  process.exit(1);
}

console.log('');

// Build the tauri command
let tauriCmd = `tauri ${command}`;
if (feature) {
  tauriCmd += ` -- --features ${feature}`;
  console.log(`🚀 Running: tauri ${command} with features: ${feature}`);
} else {
  console.log(`🚀 Running: tauri ${command} (CPU-only mode)`);
}
console.log('');

// Execute the command
try {
  execSync(tauriCmd, { stdio: 'inherit', env });
} catch (err) {
  process.exit(err.status || 1);
}
