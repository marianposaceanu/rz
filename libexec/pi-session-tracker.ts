// Managed by rz. Reinstall with rz --install-pi-extension.
import { mkdirSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { performance } from 'node:perf_hooks';
import type { ExtensionAPI, ExtensionContext } from '@earendil-works/pi-coding-agent';

export default function (pi: ExtensionAPI) {
  const configured = process.env.PI_CODING_AGENT_DIR;
  const agentDir = configured
    ? resolve(configured === '~' ? homedir() : configured.replace(/^~\//, `${homedir()}/`))
    : join(homedir(), '.pi', 'agent');
  const directory = join(agentDir, 'rz-sessions');
  const path = join(directory, `${process.pid}.json`);
  const temporary = `${path}.tmp`;
  const startedAt = Math.floor(performance.timeOrigin / 1000);

  const cleanup = () => {
    rmSync(path, { force: true });
    rmSync(temporary, { force: true });
    process.removeListener('exit', cleanup);
  };

  const track = (_event: unknown, ctx: ExtensionContext) => {
    cleanup();
    const sessionFile = ctx.sessionManager.getSessionFile();
    if (ctx.mode !== 'tui' || !process.stdin.isTTY || !sessionFile) return;

    mkdirSync(directory, { recursive: true, mode: 0o700 });
    writeFileSync(temporary, JSON.stringify({
      pid: process.pid,
      startedAt,
      sessionId: ctx.sessionManager.getSessionId(),
      sessionFile: resolve(sessionFile),
      cwd: ctx.sessionManager.getCwd(),
    }) + '\n', { mode: 0o600 });
    renameSync(temporary, path);
    process.once('exit', cleanup);
  };

  pi.on('session_start', track);
  pi.on('session_shutdown', cleanup);
}
