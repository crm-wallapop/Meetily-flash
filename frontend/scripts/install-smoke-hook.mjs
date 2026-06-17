// Self-installs .githooks via core.hooksPath on `pnpm install`; no-ops outside a git repo (Docker builds that don't copy .git).
import { execSync } from 'node:child_process';
import { chmodSync, existsSync } from 'node:fs';
import { join } from 'node:path';

function repoRoot() {
  try {
    return execSync('git rev-parse --show-toplevel', {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim();
  } catch {
    return null;
  }
}

const root = repoRoot();
if (!root) process.exit(0);

try {
  execSync('git config core.hooksPath .githooks', { cwd: root, stdio: 'ignore' });
  const hook = join(root, '.githooks', 'pre-push');
  if (existsSync(hook)) {
    try { chmodSync(hook, 0o755); } catch { /* Windows filesystems carry no exec bit */ }
  }
} catch {
  // git not on PATH — the hook stays inert until git is available.
}
