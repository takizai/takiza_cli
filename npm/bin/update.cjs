'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { spawn } = require('node:child_process');

function newer(candidate, current) {
  const parse = value => /^(\d+)\.(\d+)\.(\d+)$/.exec(value);
  const a = parse(candidate), b = parse(current);
  if (!a || !b) return false;
  for (let i = 1; i <= 3; i++) {
    if (BigInt(a[i]) !== BigInt(b[i])) return BigInt(a[i]) > BigInt(b[i]);
  }
  return false;
}

function configPaths(cwd = process.cwd(), env = process.env) {
  const home = env.HOME || env.USERPROFILE || (env.HOMEDRIVE && env.HOMEPATH && env.HOMEDRIVE + env.HOMEPATH);
  return [home && path.join(home, '.config', 'takiza', 'config.json'), path.join(cwd, '.takiza', 'config.json')].filter(Boolean);
}

function autoEnabled(paths = configPaths()) {
  if (process.env.CI || process.env.TAKIZA_NO_UPDATE === '1') return false;
  // A global opt-out cannot be overridden by a workspace. Broken config also
  // disables background installation until the user fixes it.
  return paths.every(file => {
    try { return JSON.parse(fs.readFileSync(file, 'utf8')).auto_update !== false; }
    catch (error) { return error.code === 'ENOENT'; }
  });
}

function npmCli(env = process.env) {
  const candidates = [env.npm_execpath,
    path.join(path.dirname(process.execPath), 'node_modules', 'npm', 'bin', 'npm-cli.js'),
    path.resolve(path.dirname(process.execPath), '..', 'lib', 'node_modules', 'npm', 'bin', 'npm-cli.js')];
  for (const directory of (env.PATH || env.Path || '').split(path.delimiter)) {
    for (const name of ['npm', 'npm.cmd']) {
      try {
        const resolved = fs.realpathSync(path.join(directory, name));
        candidates.push(resolved, path.join(path.dirname(resolved), 'node_modules', 'npm', 'bin', 'npm-cli.js'));
      } catch {}
    }
  }
  const found = candidates.find(file => file && path.basename(file) === 'npm-cli.js' && fs.existsSync(file));
  if (!found) throw new Error('Cannot locate npm. Run npm install -g takiza@latest using the npm that installed Takiza.');
  return found;
}

function runNpm(cli, args, inherit = false, timeout = 30000, signal) {
  return new Promise((resolve, reject) => {
    // Running npm-cli.js through Node avoids cmd.exe quoting and .cmd spawning
    // problems on Windows. Arguments never pass through a shell.
    const child = spawn(process.execPath, [cli, ...args], {
      stdio: inherit ? ['ignore', 'inherit', 'inherit'] : ['ignore', 'pipe', 'pipe'], windowsHide: true,
      env: { ...process.env, TAKIZA_NO_UPDATE: '1' }, signal,
    });
    let output = '', errorOutput = '';
    child.stdout?.on('data', data => { if (output.length < 1024 * 1024) output += data; });
    child.stderr?.on('data', data => { if (errorOutput.length < 16384) errorOutput += data; });
    const timer = setTimeout(() => { child.kill(); reject(new Error('npm update request timed out.')); }, timeout);
    child.once('error', error => { clearTimeout(timer); reject(error); });
    child.once('exit', (code, signal) => {
      clearTimeout(timer);
      if (code === 0) resolve(output.trim());
      else reject(new Error(`npm exited with ${signal || code}${errorOutput ? `: ${errorOutput.trim()}` : ''}`));
    });
  });
}

function installation(packageDir, globalRoot, platform = process.platform) {
  const directory = fs.realpathSync(packageDir);
  if (directory.split(path.sep).includes('_npx')) return { kind: 'npx' };
  const parent = path.dirname(directory);
  if (fs.existsSync(path.join(globalRoot, 'takiza')) && directory === fs.realpathSync(path.join(globalRoot, 'takiza'))) {
    const prefix = platform === 'win32' ? path.dirname(globalRoot) : path.resolve(globalRoot, '..', '..');
    return { kind: 'global', prefix };
  }
  // Keep a global installation tied to its original prefix even if npm's
  // configured prefix changed after installation.
  if (path.basename(parent) === 'node_modules' && path.basename(path.dirname(parent)) === 'lib') {
    return { kind: 'global', prefix: path.resolve(parent, '..', '..') };
  }
  const project = path.dirname(parent);
  if (path.basename(parent) === 'node_modules' && fs.existsSync(path.join(project, 'package.json'))) {
    return { kind: 'local', prefix: project };
  }
  // Windows global prefixes have node_modules directly inside the prefix.
  if (platform === 'win32' && path.basename(parent) === 'node_modules') {
    return { kind: 'global', prefix: project };
  }
  throw new Error('Cannot identify this npm installation. Update it using the original installation command.');
}

function installArgs(origin, version) {
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error('Invalid release version from npm.');
  if (origin.kind === 'npx') throw new Error('Temporary npx installation: run npx --yes takiza@latest.');
  return ['install', ...(origin.kind === 'global' ? ['--global'] : []), '--prefix', origin.prefix, `takiza@${version}`];
}

async function context(packageDir = path.resolve(__dirname, '..'), run = runNpm, cli = npmCli()) {
  const current = JSON.parse(fs.readFileSync(path.join(packageDir, 'package.json'), 'utf8')).version;
  const root = await run(cli, ['root', '--global']);
  return { cli, current, origin: installation(packageDir, root), run };
}

async function check(ctx) {
  const result = await ctx.run(ctx.cli, ['view', 'takiza@latest', 'version', '--json', '--fetch-retries=0', '--fetch-timeout=10000']);
  const version = JSON.parse(result);
  if (typeof version !== 'string' || !/^\d+\.\d+\.\d+$/.test(version)) throw new Error('npm returned an invalid stable release version.');
  return newer(version, ctx.current) ? version : undefined;
}

async function install(ctx, version) {
  console.log(`Updating Takiza ${ctx.current} → ${version} via npm…`);
  await ctx.run(ctx.cli, installArgs(ctx.origin, version), true, 30 * 60 * 1000);
  console.log(`Takiza ${version} installed. It will be used on the next launch.`);
}

async function manual() {
  const ctx = await context();
  if (ctx.origin.kind === 'npx') throw new Error('Temporary npx installation: run npx --yes takiza@latest.');
  console.log(`Checking npm for updates (installed: ${ctx.current})…`);
  const version = await check(ctx);
  if (version) await install(ctx, version);
  else console.log(`Takiza ${ctx.current} is up to date.`);
}

function startAutomatic(args) {
  if (args.some(arg => ['--help', '-h', '--version', '-V', '-v'].includes(arg)) || !autoEnabled()) return null;
  const home = process.env.HOME || process.env.USERPROFILE;
  if (!home) return null;
  const cache = path.join(home, '.config', 'takiza', 'updates', 'npm.json');
  // Serialize background checks/installations. npm itself reports a locked
  // executable if another Windows session still has the old binary open.
  const lock = `${cache}.lock`;
  let owned = false;
  const release = () => { if (owned) { owned = false; try { fs.unlinkSync(lock); } catch {} } };
  try {
    fs.mkdirSync(path.dirname(cache), { recursive: true });
    // Recover an abandoned lock only when its owner has exited.
    try {
      const owner = Number(fs.readFileSync(lock, 'utf8'));
      if (Number.isSafeInteger(owner) && owner > 0) {
        try { process.kill(owner, 0); } catch (error) { if (error.code === 'ESRCH') fs.unlinkSync(lock); }
      }
    } catch {}
    fs.writeFileSync(lock, String(process.pid), { flag: 'wx' });
    owned = true;
    process.once('exit', release);
  } catch { return null; }
  let ready;
  const controller = new AbortController();
  const backgroundNpm = (cli, args, inherit, timeout) => runNpm(cli, args, inherit, timeout, controller.signal);
  const pending = (async () => {
    try {
      let saved;
      try { saved = JSON.parse(fs.readFileSync(cache, 'utf8')); } catch {}
      const ctx = await context(undefined, backgroundNpm);
      if (ctx.origin.kind === 'npx') return;
      if (saved && saved.current === ctx.current && Date.now() - saved.checkedAt < 86400000) {
        if (newer(saved.version, ctx.current)) ready = { ctx, version: saved.version };
        return;
      }
      const version = await check(ctx);
      fs.writeFileSync(cache, JSON.stringify({ current: ctx.current, checkedAt: Date.now(), version }));
      if (version) ready = { ctx, version };
    } catch { /* Offline checks must not interrupt a conversation. */ }
  })();
  // A short session can skip the update; cancel any unfinished network check
  // so exiting the app never waits for the registry.
  return async success => {
    try {
      if (success && ready && autoEnabled()) await install(ready.ctx, ready.version);
    } catch (error) {
      console.error(`Takiza auto-update failed: ${error.message}\nRun takiza update to retry.`);
    } finally {
      controller.abort();
      await pending;
      release();
      process.removeListener('exit', release);
    }
  };
}

module.exports = { newer, configPaths, autoEnabled, npmCli, runNpm, installation, installArgs, context, check, install, manual, startAutomatic };
