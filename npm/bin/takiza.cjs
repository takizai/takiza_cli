#!/usr/bin/env node
'use strict';

const path = require('node:path');
const fs = require('node:fs');
const { spawn, execFileSync } = require('node:child_process');

function terminalCleanup() {
  if (!process.stdin.isTTY || !process.stdout.isTTY) return () => {};
  // Save the shell's terminal state before Rust changes it. Killing a child
  // skips Rust's Drop handlers, especially on Windows.
  let state;
  if (process.platform !== 'win32') {
    try {
      state = execFileSync('stty', ['-g'], { stdio: ['inherit', 'pipe', 'ignore'] }).toString().trim();
    } catch {}
  }
  let restored = false;
  return () => {
    if (restored) return;
    restored = true;
    try {
      fs.writeSync(1, '\x1b[?2026l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1015l\x1b[?2004l\x1b[0m\x1b[?25h\x1b[?1049l');
    } catch {}
    if (state) {
      try { execFileSync('stty', [state], { stdio: ['inherit', 'ignore', 'ignore'] }); } catch {}
    } else {
      try { process.stdin.setRawMode(false); } catch {}
    }
  };
}

function targetFor(platform, arch, glibc) {
  const targets = {
    'darwin-x64': 'x86_64-apple-darwin',
    'darwin-arm64': 'aarch64-apple-darwin',
    'win32-x64': 'x86_64-pc-windows-msvc',
    'linux-arm64': 'aarch64-unknown-linux-gnu',
    'linux-x64': glibc ? 'x86_64-unknown-linux-gnu' : 'x86_64-unknown-linux-musl',
  };
  return targets[`${platform}-${arch}`];
}

function main() {
  let glibc = false;
  if (process.platform === 'linux') {
    try { glibc = !!process.report.getReport().header.glibcVersionRuntime; } catch {}
    if (process.arch === 'arm64' && !glibc) {
      console.error('Takiza does not yet provide a Linux ARM64 musl binary.');
      process.exitCode = 1;
      return;
    }
  }
  const target = targetFor(process.platform, process.arch, glibc);
  if (!target) {
    console.error(`Takiza does not yet support ${process.platform}/${process.arch}.`);
    process.exitCode = 1;
    return;
  }
  const binary = path.join(__dirname, '..', 'vendor', target, process.platform === 'win32' ? 'takiza.exe' : 'takiza');
  if (!fs.existsSync(binary)) {
    console.error('Takiza binary is missing. Reinstall with: npm install -g takiza');
    process.exitCode = 1;
    return;
  }
  const restoreTerminal = terminalCleanup();
  process.once('exit', restoreTerminal);
  const child = spawn(binary, process.argv.slice(2), { stdio: 'inherit', windowsHide: false });
  const handlers = new Map();
  for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
    const handler = () => { if (!child.killed) child.kill(signal); };
    handlers.set(signal, handler);
    process.on(signal, handler);
  }
  child.once('error', error => {
    restoreTerminal();
    console.error(`Cannot start Takiza: ${error.message}`);
    process.exitCode = 1;
  });
  child.once('exit', (code, signal) => {
    restoreTerminal();
    for (const [name, handler] of handlers) process.removeListener(name, handler);
    process.exitCode = code ?? ({ SIGINT: 130, SIGTERM: 143, SIGHUP: 129 }[signal] || 1);
  });
}

module.exports = { targetFor };
if (require.main === module) main();
