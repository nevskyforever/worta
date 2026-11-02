#!/usr/bin/env node

import { readFile, writeFile } from 'node:fs/promises';

const [configPath, profile] = process.argv.slice(2);
if (!configPath || !profile) {
  console.error('Usage: apply-tauri-build-profile.mjs CONFIG_PATH PROFILE');
  process.exit(2);
}

const config = JSON.parse(await readFile(configPath, 'utf8'));
if (profile === 'test') {
  config.identifier = 'app.nfprogress.tracker.test';
  for (const window of config.app?.windows ?? []) {
    window.title = `${config.productName} Test`;
  }
} else if (profile !== 'production') {
  console.error(`Unsupported build profile: ${profile}`);
  process.exit(2);
}

await writeFile(configPath, `${JSON.stringify(config, null, 2)}\n`, 'utf8');
