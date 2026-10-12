import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, rm } from 'node:fs/promises';
import { createServer } from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';

// Run these checks explicitly. Each check opens a tab through the operating system.
for (const mode of ['open', 'launch']) {
  test(`${mode} delivers its workspace address through the system browser`, async (t) => {
    const executable = path.resolve('target/debug', process.platform === 'win32' ? 'gifgun-agent.exe' : 'gifgun-agent');
    const cache = await mkdtemp(path.join(os.tmpdir(), 'gifgun-browser-launch-'));
    let sessionId;
    let origin;
    let receive;
    const received = new Promise(resolve => { receive = resolve; });
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
    const start = (args, interactive = false) => {
      const child = spawn(executable, args, {
        env: { ...process.env, GIFGUN_AGENT_CACHE_DIR: cache },
        stdio: [interactive ? 'pipe' : 'ignore', 'pipe', 'pipe'],
        signal: t.signal,
      });
      let output = '';
      child.stdout.on('data', chunk => { output += chunk.toString('utf8'); });
      child.stderr.resume();
      const closed = new Promise((resolve, reject) => {
        child.on('error', () => reject(new Error('The native process could not start.')));
        child.on('close', code => resolve(code));
      });
      return { child, closed, output: () => output };
    };
    const run = async (args) => {
      const command = start(args);
      assert.equal(await command.closed, 0, 'The native command failed.');
      return JSON.parse(command.output());
    };
    let console;
    try {
      server.listen(0, '127.0.0.1');
      await once(server, 'listening');
      origin = `http://127.0.0.1:${server.address().port}`;
      let launched;
      if (mode === 'launch') {
        console = start(['launch', '--app-url', `${origin}/app`], true);
        const address = await Promise.race([
          received,
          console.closed.then(() => { throw new Error('The console closed before the browser reached its workspace.'); }),
        ]);
        sessionId = address.searchParams.get('agentSession');
        launched = { sessionId, editorUrl: address.href, workspaceId: address.searchParams.get('workspace'), port: Number(address.searchParams.get('agentPort')) };
      } else {
        launched = await run(['open', '--app-url', `${origin}/app`]);
        sessionId = launched.sessionId;
        assert.equal(launched.status, 'waiting');
      }
      assert.equal(typeof sessionId, 'string');
      const address = await received;
      assert.equal(address.origin, origin);
      assert.equal(address.href, launched.editorUrl);
      assert.equal(address.searchParams.get('workspace'), launched.workspaceId);
      assert.equal(address.searchParams.get('agentSession'), sessionId);
      assert.equal(address.searchParams.get('agentPort'), String(launched.port));
      assert.deepEqual([...address.searchParams.keys()].sort(), ['ae', 'agentPort', 'agentSession', 'workspace']);
      const status = await run(['--session', sessionId, 'status']);
      assert.equal(status.sessionId, sessionId);
      assert.equal(status.browserConnected, false);
    } finally {
      try {
        if (console) {
          console.child.stdin.end('\n');
          assert.equal(await console.closed, 0);
          assert.ok(console.output().includes(`Workspace: ${sessionId}`));
          assert.ok(console.output().includes('This bridge has stopped.'));
        } else if (sessionId) {
          const stopped = await run(['--session', sessionId, 'disconnect']);
          assert.equal(stopped.status, 'disconnected');
        }
      } finally {
        await new Promise(resolve => server.close(resolve));
        await rm(cache, { recursive: true, force: true });
      }
    }
  });
}
