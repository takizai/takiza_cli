'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { targetFor } = require('../bin/takiza.cjs');

test('selects the binary for each supported platform', () => {
  assert.equal(targetFor('linux', 'x64', true), 'x86_64-unknown-linux-gnu');
  assert.equal(targetFor('linux', 'x64', false), 'x86_64-unknown-linux-musl');
  assert.equal(targetFor('linux', 'arm64', true), 'aarch64-unknown-linux-gnu');
  assert.equal(targetFor('darwin', 'x64'), 'x86_64-apple-darwin');
  assert.equal(targetFor('darwin', 'arm64'), 'aarch64-apple-darwin');
  assert.equal(targetFor('win32', 'x64'), 'x86_64-pc-windows-msvc');
  assert.equal(targetFor('win32', 'arm64'), undefined);
  assert.equal(targetFor('linux', 'ia32'), undefined);
});

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const update = require('../bin/update.cjs');

function temporary(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'takiza-update-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

test('stable version comparison never downgrades or accepts command text', () => {
  assert.equal(update.newer('0.1.12', '0.1.11'), true);
  assert.equal(update.newer('0.10.0', '0.9.99'), true);
  for (const version of ['0.1.10', '0.1.11', '1.0.0-beta.1', 'latest', '1.0.0;evil']) {
    assert.equal(update.newer(version, '0.1.11'), false);
  }
  assert.throws(() => update.installArgs({ kind: 'global', prefix: '/tmp/a b' }, '1.0.0;evil'));
  assert.deepEqual(update.installArgs({ kind: 'global', prefix: 'C:\\my npm' }, '0.1.12'),
    ['install', '--global', '--prefix', 'C:\\my npm', 'takiza@0.1.12']);
});

test('global and workspace config can each disable updates; invalid JSON fails closed', t => {
  const dir = temporary(t);
  const global = path.join(dir, 'global.json'), local = path.join(dir, 'local.json');
  const originalCI = process.env.CI;
  delete process.env.CI;
  t.after(() => { if (originalCI === undefined) delete process.env.CI; else process.env.CI = originalCI; });
  assert.equal(update.autoEnabled([global, local]), true);
  fs.writeFileSync(global, '{"auto_update":false}');
  fs.writeFileSync(local, '{"auto_update":true}');
  assert.equal(update.autoEnabled([global, local]), false);
  fs.writeFileSync(global, '{}');
  fs.writeFileSync(local, '{"auto_update":false}');
  assert.equal(update.autoEnabled([global, local]), false);
  fs.writeFileSync(local, '{');
  assert.equal(update.autoEnabled([global, local]), false);
});

test('preserves original npm prefix, detects local installs and npx', t => {
  const dir = temporary(t);
  const packageDir = path.join(dir, 'original prefix/lib/node_modules/takiza');
  fs.mkdirSync(packageDir, { recursive: true });
  assert.deepEqual(update.installation(packageDir, path.join(dir, 'different/node_modules')),
    { kind: 'global', prefix: path.join(dir, 'original prefix') });
  const project = path.join(dir, 'project');
  const local = path.join(project, 'node_modules/takiza');
  fs.mkdirSync(local, { recursive: true });
  fs.writeFileSync(path.join(project, 'package.json'), '{}');
  assert.deepEqual(update.installation(local, path.join(dir, 'absent')), { kind: 'local', prefix: project });
  const npx = path.join(dir, '_npx/hash/node_modules/takiza');
  fs.mkdirSync(npx, { recursive: true });
  assert.deepEqual(update.installation(npx, path.join(dir, 'absent')), { kind: 'npx' });
  assert.throws(() => update.installArgs({ kind: 'npx' }, '0.1.12'), /npx --yes takiza@latest/);
});

test('registry check and install use the same npm and pinned version; failures propagate', async t => {
  const dir = temporary(t);
  const packageDir = path.join(dir, 'lib/node_modules/takiza');
  fs.mkdirSync(packageDir, { recursive: true });
  fs.writeFileSync(path.join(packageDir, 'package.json'), '{"version":"0.1.11"}');
  const calls = [];
  const run = async (cli, args) => {
    calls.push({ cli, args });
    if (args[0] === 'root') return path.dirname(packageDir);
    if (args[0] === 'view') return '"0.1.12"';
    throw new Error('permission denied');
  };
  const ctx = await update.context(packageDir, run, 'original/npm-cli.js');
  assert.equal(await update.check(ctx), '0.1.12');
  await assert.rejects(update.install(ctx, '0.1.12'), /permission denied/);
  assert.equal(calls[2].cli, calls[0].cli);
  assert.deepEqual(calls[2].args, ['install', '--global', '--prefix', dir, 'takiza@0.1.12']);
});

// Exercise the real launcher with a fake npm executable. No network requests or
// changes to an installed Takiza are made by this test.
test('manual and automatic npm updates run after the binary exits, opt-out makes no npm calls', { skip: process.platform === 'win32' }, t => {
  const dir = temporary(t);
  const packageDir = path.join(dir, 'prefix/lib/node_modules/takiza');
  const bin = path.join(packageDir, 'bin');
  fs.mkdirSync(bin, { recursive: true });
  for (const name of ['takiza.cjs', 'update.cjs']) fs.copyFileSync(path.join(__dirname, '../bin', name), path.join(bin, name));
  fs.writeFileSync(path.join(packageDir, 'package.json'), '{"version":"0.1.11"}');
  const target = targetFor(process.platform, process.arch, !!process.report.getReport().header.glibcVersionRuntime);
  const vendor = path.join(packageDir, 'vendor', target);
  fs.mkdirSync(vendor, { recursive: true });
  const log = path.join(dir, 'calls.jsonl');
  const exited = path.join(dir, 'exited');
  fs.writeFileSync(path.join(vendor, 'takiza'), `#!/usr/bin/env node\nsetTimeout(() => { require('fs').writeFileSync(${JSON.stringify(exited)}, 'yes'); }, 800);\n`, { mode: 0o755 });
  const cli = path.join(dir, 'npm-cli.js');
  fs.writeFileSync(cli, `
    const fs = require('fs');
    const args = process.argv.slice(2);
    fs.appendFileSync(${JSON.stringify(log)}, JSON.stringify({ args, exited: fs.existsSync(${JSON.stringify(exited)}) }) + '\\n');
    if (args[0] === 'root') console.log(${JSON.stringify(path.dirname(packageDir))});
    else if (args[0] === 'view') console.log('"0.1.12"');
  `);
  const configDir = path.join(dir, 'home/.config/takiza');
  fs.mkdirSync(configDir, { recursive: true });
  const config = path.join(configDir, 'config.json');
  const env = { ...process.env, HOME: path.join(dir, 'home'), USERPROFILE: path.join(dir, 'home'), npm_execpath: cli, CI: '', TAKIZA_NO_UPDATE: '' };
  const launch = args => {
    const result = spawnSync(process.execPath, [path.join(bin, 'takiza.cjs'), ...args], { env, cwd: dir, encoding: 'utf8', timeout: 10000 });
    assert.equal(result.status, 0, result.stderr || result.error?.message);
    return result;
  };
  fs.writeFileSync(config, '{"auto_update":false}');
  launch([]);
  assert.equal(fs.existsSync(log), false);
  fs.unlinkSync(exited);
  launch(['update']); // explicit update ignores the automatic opt-out
  let calls = fs.readFileSync(log, 'utf8').trim().split('\n').map(JSON.parse);
  assert.deepEqual(calls.map(call => call.args[0]), ['root', 'view', 'install']);
  assert.equal(fs.existsSync(exited), false); // no TUI started by the subcommand
  fs.unlinkSync(log);
  fs.writeFileSync(config, '{"auto_update":true}');
  launch([]);
  calls = fs.readFileSync(log, 'utf8').trim().split('\n').map(JSON.parse);
  assert.deepEqual(calls.map(call => call.args[0]), ['root', 'view', 'install']);
  assert.equal(calls[2].exited, true);
  fs.unlinkSync(log);
  launch([]);
  calls = fs.readFileSync(log, 'utf8').trim().split('\n').map(JSON.parse);
  assert.deepEqual(calls.map(call => call.args[0]), ['root', 'install']); // cached check
});
