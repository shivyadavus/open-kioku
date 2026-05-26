#!/usr/bin/env node

const fs = require('fs');
const path = require('path');
const https = require('https');
const os = require('os');
const { execSync } = require('child_process');

const VERSION = require('../package.json').version;
const REPO = "shivyadavus/open-kioku";

const ARCH_MAP = {
    'x64': 'x86_64',
    'arm64': 'arm64',
};

const OS_MAP = {
    'darwin': 'macos',
    'linux': 'linux',
    'win32': 'windows',
};

async function download(url, dest) {
    return new Promise((resolve, reject) => {
        const file = fs.createWriteStream(dest);
        https.get(url, (response) => {
            if (response.statusCode === 302 || response.statusCode === 301) {
                download(response.headers.location, dest).then(resolve).catch(reject);
                return;
            }
            if (response.statusCode !== 200) {
                reject(new Error(`Failed to download: ${response.statusCode} ${response.statusMessage}`));
                return;
            }
            response.pipe(file);
            file.on('finish', () => {
                file.close();
                resolve();
            });
        }).on('error', (err) => {
            fs.unlink(dest, () => reject(err));
        });
    });
}

async function install() {
    const osType = OS_MAP[os.platform()];
    const archType = ARCH_MAP[os.arch()];

    if (!osType || !archType) {
        console.error(`Unsupported platform: ${os.platform()} ${os.arch()}`);
        console.error(`Please install from source: cargo install --path crates/open-kioku-cli`);
        process.exit(1);
    }

    // Windows has .exe
    const ext = osType === 'windows' ? '.exe' : '';
    const binaryName = `ok-${osType}-${archType}${ext}`;
    const url = `https://github.com/${REPO}/releases/download/v${VERSION}/${binaryName}`;
    
    const binDir = path.join(__dirname, '..', 'bin');
    const dest = path.join(binDir, `ok${ext}`);

    console.log(`Downloading Open Kioku v${VERSION} for ${osType}-${archType}...`);
    try {
        await download(url, dest);
        if (osType !== 'windows') {
            fs.chmodSync(dest, 0o755);
        }
        console.log(`Successfully installed Open Kioku.`);
    } catch (err) {
        console.error(`Failed to install Open Kioku binary from GitHub Releases: ${err.message}`);
        console.error(`URL: ${url}`);
        console.error(`\nYou can still install manually using cargo:\n  cargo install --git https://github.com/${REPO}`);
        process.exit(1);
    }
}

install();
