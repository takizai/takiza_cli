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
