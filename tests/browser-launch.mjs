import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, rm } from 'node:fs/promises';
import { createServer } from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';

// Run this check explicitly. It opens a tab through the operating system.
test('the system browser receives the managed workspace address', async (t) => {
  const executable = path.resolve('target/debug', process.platform === 'win32' ? 'gifgun-agent.exe' : 'gifgun-agent');
  const cache = await mkdtemp(path.join(os.tmpdir(), 'gifgun-browser-launch-'));
  let sessionId;
  let origin;
  let receive;
  const received = new Promise((resolve) => { receive = resolve; });
  const server = createServer((request, response) => {
    const address = new URL(request.url, origin);
    if (address.pathname !== '/app') {
      response.writeHead(404).end();
      return;
    }
    response.writeHead(200, {
      'Content-Type': 'text/html; charset=utf-8',
      'Content-Security-Policy': "default-src 'none'",
      'Connection': 'close',
    });
    response.end('<!doctype html><title>GifGun browser launch check</title><p>The browser launch check passed. You can close this tab.</p>');
    receive(address);
  });
  const run = (args) => new Promise((resolve, reject) => {
    const child = spawn(executable, args, {
      env: { ...process.env, GIFGUN_AGENT_CACHE_DIR: cache },
      stdio: ['ignore', 'pipe', 'pipe'],
      signal: t.signal,
    });
    const stdout = [];
    child.stdout.on('data', (chunk) => stdout.push(chunk));
    child.stderr.resume();
    child.on('error', () => reject(new Error('The native process could not start.')));
    child.on('close', (code) => {
      if (code !== 0) {
        reject(new Error('The native browser launch check failed.'));
        return;
      }
      try {
        resolve(JSON.parse(Buffer.concat(stdout).toString('utf8')));
      } catch {
        reject(new Error('The native response was not valid JSON.'));
      }
    });
  });
  try {
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');
    origin = `http://127.0.0.1:${server.address().port}`;
    const launched = await run(['open', '--app-url', `${origin}/app`]);
    sessionId = launched.sessionId;
    assert.equal(typeof sessionId, 'string');
    assert.equal(launched.status, 'waiting');
    const address = await received;
    assert.equal(address.origin, origin);
    assert.equal(address.href, launched.editorUrl);
    assert.equal(address.searchParams.get('workspace'), launched.workspaceId);
    assert.equal(address.searchParams.get('agentSession'), sessionId);
    assert.equal(address.searchParams.get('agentPort'), String(launched.port));
    assert.deepEqual([...address.searchParams.keys()].sort(), ['ae', 'agentPort', 'agentSession', 'workspace']);
  } finally {
    try {
      if (sessionId) {
        const stopped = await run(['--session', sessionId, 'disconnect']);
        assert.equal(stopped.status, 'disconnected');
      }
    } finally {
      await new Promise((resolve) => server.close(resolve));
      await rm(cache, { recursive: true, force: true });
    }
  }
});
