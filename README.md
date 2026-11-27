<div align="center">

<img src="app/src-tauri/icons/128x128.png" width="88" height="88" alt="Ratspeak Agent logo">

# Ratspeak Agent

Host a private AI assistant you can message over [Ratspeak](https://github.com/ratspeak/Ratspeak). Open **Ratspeak Agent** on macOS, Windows, or Linux to run an agent on your computer or an SD card on your Raspberry Pi.

[Ratspeak](https://github.com/ratspeak/Ratspeak) |
[Docs](https://docs.ratspeak.org/) |
[rsReticulum](https://github.com/ratspeak/rsReticulum) |
[rsLXMF](https://github.com/ratspeak/rsLXMF) |
[rsLXST](https://github.com/ratspeak/rsLXST)

[![License: Reticulum License](https://img.shields.io/badge/license-Reticulum%20License-blue.svg)](LICENSE)
[![Rust 1.96](https://img.shields.io/badge/rust-1.96-orange.svg)](https://www.rust-lang.org)
[![Status](https://img.shields.io/badge/status-public%20beta-green.svg)](#current-state)

</div>

## What It Is

Ratspeak Agent turns a computer or Raspberry Pi into an account-free, end-to-end encrypted assistant reachable through Ratspeak. Message it over the same local network or Wi-Fi, optional public hubs (or host your own), and any other entry into the mesh that Ratspeak supports.

It runs on [rsReticulum](https://github.com/ratspeak/rsReticulum) and [rsLXMF](https://github.com/ratspeak/rsLXMF), Rust ports of [Reticulum](https://github.com/markqvist/Reticulum) and [LXMF](https://github.com/markqvist/LXMF). Every agent generates its own Reticulum identity locally and starts locked until you claim it by scanning its QR code and sending its rotating six-character setup code.

Choose where inference runs:

- **Local models** run directly on your computer (up to Qwen3 32B) or Raspberry Pi (board-matched models from SmolLM2 360M to Qwen3.5 4B), with zero cloud dependency.
- **Venice** connects your own Venice API key to private text (and vision-capable) models when internet access is available.

## Current State

Ratspeak Agent is in **public beta**. Core desktop and Raspberry Pi flows work end-to-end, but expect rough edges and platform gaps while beta testing continues. If something breaks or feels off, open an issue with your OS, hardware, and steps to reproduce. For suspected security issues, report privately via [SECURITY.md](SECURITY.md).

- **Verified:** macOS desktop & physical Raspberry Pi 5 flashing/chat, Linux x64/arm64 builds and desktop tray/lifecycle flows, offline libraries, translation packs, and arm64 container installs.
- **Still being tested:** Interactive Windows (WebView2) & physical SD card writes, physical SD card writes on Linux, live phone-to-agent voice notes, live two-agent A2A file transfers, and non-Pi-5 speed benchmarks.
- **Private beta opt-in:** Agent-to-Agent (A2A) contacts sit behind a lightweight participation gate inside the app while that flow matures.

## What You Get

- **Guided desktop & Pi setup:** One app for macOS, Windows, and Linux to host Docker-isolated desktop agents or flash and later reconfigure Raspberry Pi SD cards.
- **Multi-agent management:** Run multiple desktop agents side by side, rename or stop/start them, keep them running from the system tray, and update without losing identities or chat history.
- **Owner-first access control:** Rotating 6-character pairing codes (new every minute, rate-limited per sender), private-by-default inbox, owner-priority moderation commands (`/allow`, `/revoke`, `/anyone`, `/private`, `/status`, `/model`), and bounded queuing when public access is enabled.
- **Built-in offline tools:** Calculator (`/calc`), unit conversion (`/convert`), world places (`/where`, `/distance`, `/time`), sunrise/sunset/moon phases (`/sun`, `/moon`), and plain-text file attachments.
- **Optional offline extras:**
  - **Voice messages:** English speech-to-text for Ratspeak Opus and Codec2 700C clips (Whisper `base.en` / `tiny.en` or Vosk).
  - **Offline libraries:** Kiwix editions of Wikipedia, Wikipedia Medicine, Wiktionary, and Outdoors & Ham Radio Stack Exchange — plus `/view` to export any lookup as a compact PDF.
  - **Offline translation (Raspberry Pi):** Ten languages to/from English via Argos Translate packs (Spanish, French, German, Italian, Portuguese, Arabic, Chinese, Japanese, Ukrainian, and Hindi) for small Pi models; larger models translate directly.
- **Private-beta A2A exchanges:** Owner-reviewed agent-to-agent requests and replies with optional signed SHA-256 file attachments (up to 900 KB) and optional AI drafting.

## Getting Started

Download **Ratspeak Agent** for your platform from the [Releases](https://github.com/ratspeak/ratspeak-agent/releases) page, or [build from source](#build-from-source).

For the Linux x86-64 AppImage, make the download executable and open it. If FUSE is unavailable, set `APPIMAGE_EXTRACT_AND_RUN=1` when launching it from a terminal. Keep the file in a writable folder for in-app updates.

In the app, select **Check for updates** and then **Update now** when an update is available. The app verifies the download, installs it, and reopens. Your system may ask for permission to install it.

### Host a Desktop Agent

1. Open **Ratspeak Agent** and choose **Desktop Agent** (it helps install Docker if it isn't present).
2. Name your agent, choose a local model or Venice, pick connections (Local Network is enabled automatically; public hubs are optional), and pick any extras.
3. In **Pairing**, scan the contact QR in Ratspeak (**Contacts → Add contact → From QR code**) and send the current six-character setup code.

### Set Up a Raspberry Pi Agent

You need a **Raspberry Pi 3, 4, 5, or Zero 2 W** and an **8 GB+ SD card**.

1. Open **Ratspeak Agent**, choose **Raspberry Pi Agent**, and insert your SD card (only removable memory cards of 8–600 GB are offered, never system or external drives).
2. Pick your board/RAM, model, Wi-Fi / Ethernet / offline setup, hubs, and extras, then confirm erasing the card. Ratspeak Agent downloads and verifies Raspberry Pi OS Lite (Trixie, 64-bit), stages the agent and settings, writes the card, and reads it back.
3. Boot the Pi, scan its QR in Ratspeak, and send the current setup code once ready. Reinsert the SD card into your computer anytime to change Wi-Fi, hubs, model, extras, access, or to erase chats/reset identity.

### Install Directly on a Raspberry Pi

On a Pi already running 64-bit Raspberry Pi OS (Debian 13 "Trixie" or later), unpack the `ratspeak-pi-*-aarch64.tar.gz` release archive and run:

```sh
sudo ./install.sh
```

For unattended setup: `sudo ./install.sh --yes --name "Kitchen Pi" --owner <your-32-hex-address>` (`--help` lists all flags). Check status anytime with `ratspeak-pi status`, or print a fresh pairing code with `sudo ratspeak-pi pairing-code`.

## Models & Commands

On a Raspberry Pi, Ratspeak Agent picks the strongest model that fits your board and memory; smaller options stay available if you prefer faster replies.

| Board & Memory | Recommended Model |
| --- | --- |
| Pi 3 A+ or Zero 2 W (512 MB) | SmolLM2 360M |
| Pi 3 B / B+, Pi 4, or Pi 5 (1 GB) | LFM2.5 1.2B |
| Pi 4 (2–8 GB) or Pi 5 (2 GB) | Qwen3.5 2B |
| Pi 5 (4–16 GB) | Qwen3.5 4B |

Pinned URLs and SHA-256 checksums for every model, library, voice engine, and translation pack live in [models.json](ratspeak_pi/models.json), [resources.json](ratspeak_pi/resources.json), [voice.json](ratspeak_pi/voice.json), and [translations.json](ratspeak_pi/translations.json).

**Chat commands** (`/help` lists what is enabled):
`/calc` · `/convert` · `/where` · `/distance` · `/time` · `/sun` · `/moon` · `/translate` · `/wiki` · `/med` · `/define` · `/outdoors` · `/ham` · `/view`

**Owner commands** (answered ahead of queued chat):
`/allow <address>` · `/revoke <address>` · `/anyone` · `/private` · `/status` · `/model [list|id]`

Each Ratspeak conversation tab has its own model context. The owner can send `/clear` or `/clearcontext` in any tab to start fresh there and keep the chat transcript.

You can attach a UTF-8 text file or a ZIP of supported text files such as `.txt`, `.md`, `.csv`, and `.json`. A ZIP can contain up to 64 entries. The agent reads up to 64 KB of decompressed text in memory and sends an excerpt that fits the model's context. For a large archive, send the relevant files or excerpts with your question.

Attach a PDF with selectable text and send your question with it. For larger PDFs, the agent searches the text and sends relevant passages with PDF page numbers. It processes up to 200 pages or 512,000 characters of extracted text. Scans without a text layer need OCR before sending.

PDF reading uses Poppler's `pdftotext` from `poppler-utils`. Desktop containers and Raspberry Pi installs that use APT install the package automatically. Have it installed on Raspberry Pi OS before an offline setup if you want PDF reading there.

## Build From Source

Clone [rsReticulum](https://github.com/ratspeak/rsReticulum), [rsLXMF](https://github.com/ratspeak/rsLXMF), and [rsLXST](https://github.com/ratspeak/rsLXST) alongside this repository and fetch their release tags. Building requires Rust 1.96, [Tauri 2](https://tauri.app) (`cargo install tauri-cli --version "^2" --locked`), Python 3.11+, and Docker for the native transport binaries.

```sh
python3 scripts/check-upstream-pins.py
scripts/build-native.sh
scripts/package.sh
scripts/build-app.sh
```

Linux packages use host tools `curl`, `util-linux`, `polkit`/`pkexec`, `udisks2`, and `xdg-utils`. Linux AppImage builds also require `patchelf` and the Ayatana AppIndicator development package.

## License

Ratspeak Agent is licensed under the [Reticulum License](LICENSE), Copyright (c) 2026 Ratspeak LLC. It uses rsReticulum and rsLXMF, ports of Mark Qvist's Reticulum and LXMF reference implementations that carry his copyright under the same license.

Voice message decoding in the native transport uses rsLXST (LXST Copyright 2026 Mark Qvist), which is licensed strictly for non-commercial use within Ratspeak only: no commercial use, no derivatives, and no use outside Ratspeak are permitted. Any build that includes rsLXST is non-commercial. Full notices and third-party terms are in [NOTICE](NOTICE).
