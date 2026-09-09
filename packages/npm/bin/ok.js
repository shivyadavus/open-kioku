#!/usr/bin/env node

const { spawnSync } = require('child_process');
const path = require('path');
const os = require('os');
const fs = require('fs');

const ARCH_MAP = {
    'x64': 'x64',
    'arm64': 'arm64',
};

const OS_MAP = {
    'darwin': 'darwin',
    'linux': 'linux',
    'win32': 'win32',
};

function getBinaryPath() {
    if (os.platform() === 'darwin' && os.arch() === 'x64') {
        console.error('Open Kioku ships no prebuilt macOS Intel binary on npm, GitHub releases, cargo-binstall, or Homebrew. Build from source with `cargo install open-kioku-cli`, or install the last release that shipped an Intel binary: `npm install -g open-kioku@2.4.0`.');
        process.exit(1);
    }

    const osType = OS_MAP[os.platform()];
    const archType = ARCH_MAP[os.arch()];

    if (!osType || !archType) {
        console.error(`Unsupported platform: ${os.platform()} ${os.arch()}`);
        process.exit(1);
    }

    const packageName = `@open-kioku/${osType}-${archType}`;
    
    try {
        const packageJsonPath = require.resolve(`${packageName}/package.json`);
        const packageDir = path.dirname(packageJsonPath);
        const ext = osType === 'win32' ? '.exe' : '';
        const binPath = path.join(packageDir, `ok${ext}`);
        
        if (!fs.existsSync(binPath)) {
            console.error(`Error: Binary not found at expected path: ${binPath}`);
            process.exit(1);
        }
        return binPath;
    } catch (err) {
        console.error(`Error: Failed to resolve optional dependency ${packageName}.`);
        console.error(`Please ensure you installed open-kioku properly, and that your package manager supports optionalDependencies.`);
        process.exit(1);
    }
}

const binPath = getBinaryPath();
const args = process.argv.slice(2);
const result = spawnSync(binPath, args, { stdio: 'inherit' });

if (result.error) {
    console.error(`Failed to execute binary: ${result.error.message}`);
    process.exit(1);
}

process.exit(result.status !== null ? result.status : 1);
