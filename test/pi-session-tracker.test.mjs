import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { performance } from 'node:perf_hooks';
import { test } from 'node:test';
import tracker from '../libexec/pi-session-tracker.ts';

function setup(t) {
  const directory = mkdtempSync(join(tmpdir(), 'rz-pi-test-'));
  const previousDir = process.env.PI_CODING_AGENT_DIR;
  const previousTTY = Object.getOwnPropertyDescriptor(process.stdin, 'isTTY');
  process.env.PI_CODING_AGENT_DIR = directory;
  Object.defineProperty(process.stdin, 'isTTY', { value: true, configurable: true });
  const handlers = new Map();
  tracker({ on: (event, handler) => handlers.set(event, handler) });
  t.after(() => {
    handlers.get('session_shutdown')();
    if (previousDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
    else process.env.PI_CODING_AGENT_DIR = previousDir;
    if (previousTTY) Object.defineProperty(process.stdin, 'isTTY', previousTTY);
    else delete process.stdin.isTTY;
    rmSync(directory, { recursive: true, force: true });
  });
  const path = join(directory, 'rz-sessions', `${process.pid}.json`);
  return {
    path,
    start(id, file, mode = 'tui') {
      handlers.get('session_start')({}, {
        mode,
        sessionManager: {
          getSessionId: () => id,
          getSessionFile: () => file,
          getCwd: () => '/tmp/project',
        },
      });
    },
    shutdown: () => handlers.get('session_shutdown')(),
  };
}

test('records the exact Pi session, including custom paths and IDs', (t) => {
  const runtime = setup(t);
  runtime.start('custom-id', "/tmp/custom sessions/work's session.jsonl");
  const record = JSON.parse(readFileSync(runtime.path, 'utf8'));
  assert.equal(record.pid, process.pid);
  assert.equal(record.sessionId, 'custom-id');
  assert.equal(record.sessionFile, "/tmp/custom sessions/work's session.jsonl");
  assert.equal(record.cwd, '/tmp/project');
  assert.ok(Math.abs(record.startedAt - performance.timeOrigin / 1000) < 1);
  assert.equal(statSync(runtime.path).mode & 0o777, 0o600);
  assert.equal(existsSync(`${runtime.path}.tmp`), false);
});

test('tracks the newly selected session after a switch, fork, or reload', (t) => {
  const runtime = setup(t);
  runtime.start('first', '/tmp/first.jsonl');
  runtime.shutdown();
  assert.equal(existsSync(runtime.path), false);
  runtime.start('second', '/tmp/second.jsonl');
  assert.equal(JSON.parse(readFileSync(runtime.path, 'utf8')).sessionId, 'second');
  runtime.shutdown();
  runtime.start('second', '/tmp/second.jsonl');
  assert.equal(JSON.parse(readFileSync(runtime.path, 'utf8')).sessionFile, '/tmp/second.jsonl');
});

test('never advertises ephemeral, RPC, or noninteractive sessions', (t) => {
  const runtime = setup(t);
  runtime.start('saved', '/tmp/saved.jsonl');
  runtime.start('ephemeral', undefined);
  assert.equal(existsSync(runtime.path), false);
  for (const mode of ['rpc', 'json', 'print']) {
    runtime.start('background', '/tmp/background.jsonl', mode);
    assert.equal(existsSync(runtime.path), false);
  }
  Object.defineProperty(process.stdin, 'isTTY', { value: false, configurable: true });
  runtime.start('not-a-terminal', '/tmp/not-a-terminal.jsonl');
  assert.equal(existsSync(runtime.path), false);
});
