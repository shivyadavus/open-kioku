#!/usr/bin/env node

const { spawnSync } = require('child_process');
const path = require('path');
const os = require('os');
const fs = require('fs');

const ext = os.platform() === 'win32' ? '.exe' : '';
const binPath = path.join(__dirname, `ok${ext}`);

if (!fs.existsSync(binPath)) {
    console.error(`Error: Open Kioku binary not found at ${binPath}`);
    console.error(`The postinstall script may have failed. Please run 'npm install -g open-kioku' again or install via Cargo.`);
    process.exit(1);
}

const args = process.argv.slice(2);
const result = spawnSync(binPath, args, { stdio: 'inherit' });

if (result.error) {
    console.error(`Failed to execute binary: ${result.error.message}`);
    process.exit(1);
}

process.exit(result.status !== null ? result.status : 1);
