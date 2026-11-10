<div align="center">

# Sonivra

Live transcription and translated subtitles for Windows, powered by local models

<p>
<img src="https://img.shields.io/badge/Rust-2024-000000?style=flat-square&logo=rust&logoColor=white" alt="Rust 2024">
<img src="https://img.shields.io/badge/Slint-1.18-2379F4?style=flat-square" alt="Slint 1.18">
<img src="https://img.shields.io/badge/Windows-11-0078D4?style=flat-square&logo=windows&logoColor=white" alt="Windows 11">
<img src="https://img.shields.io/badge/llama.cpp-CUDA-76B900?style=flat-square&logo=nvidia&logoColor=white" alt="llama.cpp CUDA">
<img src="https://img.shields.io/badge/speech.cpp-Vulkan-AC162C?style=flat-square&logo=vulkan&logoColor=white" alt="speech.cpp Vulkan">
<img src="https://img.shields.io/badge/License-GPL--3.0-blue?style=flat-square" alt="GPL-3.0-only">
</p>

[繁體中文](README.md) ・ [English](README.en.md) ・ [日本語](README.ja.md)

<img src="docs/overlay.png" alt="Subtitle window: the newest line in large type, earlier lines fading in order" width="720">

</div>

## What it does

Sonivra turns the audio playing on your PC into live subtitles. Keep the floating subtitle window over a video, a livestream or another app, and read the transcript, the translation or both.

You can choose the spoken language and the translation language separately. Japanese to Traditional Chinese (Taiwan) is the default and the pair tested most closely so far. Other language pairs are available, with support depending on the models you choose.

Recognition and translation run on your PC by default. In this mode, your audio and transcripts stay local. If you configure an external translation endpoint, text is sent to that endpoint instead.

The app is built with Rust and [Slint](https://slint.dev/). It needs no Python or Node.js installation, Electron or WebView.

## Download and setup

1. Download the Windows ZIP from [Releases](https://github.com/HsuanC0921/sonivra/releases/latest) and extract it.
2. Run `Sonivra.exe`.
3. Follow the setup page to download the inference runtimes and models. These are separate downloads with pinned versions and SHA-256 verification.
4. Choose your audio source, spoken language and translation language, then start listening.

The ZIP includes a file named `portable`. While that file is beside the executable, Sonivra keeps its settings, models and transcripts there too. Without it, Sonivra uses `%LOCALAPPDATA%\Sonivra`.

### System requirements

- Windows 11 x64
- An NVIDIA GPU is recommended for the default setup, which uses CUDA for translation and Vulkan for speech recognition
- About 2.9 GB of disk space for the default model pair, plus space for the runtimes

The defaults are Qwen3-ASR 1.7B Q4_K and PLaMo 3 Translate 2B Q4_K_M. Download size is not a measure of RAM or VRAM use. Measurements from one test machine are listed below.

## Subtitles and controls

<img src="docs/settings.png" alt="Subtitle appearance settings with a live preview" width="720">

The subtitle window stays on top of other windows. Drag its title bar to move it, or an edge or corner to resize it. You can change the font, text size, colors, background opacity and text effects. Older lines fade as new ones arrive, with up to 300 lines retained for display.

Scroll to read earlier subtitles. After 20 seconds without input, the window returns to the live view. Hold Ctrl while scrolling to change the text size, or Shift to change the background opacity.

Right-click to open settings. The title bar can stay visible, appear on hover or stay hidden. Lock the window to enable click-through, so mouse clicks reach the app beneath it.

| Shortcut | Action |
|---|---|
| `Ctrl+Alt+L` | Lock or unlock the window |
| `Ctrl+Alt+P` | Start, pause or resume |
| `Ctrl+Alt+H` | Show or hide subtitles |
| `Ctrl+Alt+↑` / `↓` | Scroll through earlier or later subtitles |
| `Ctrl+Alt+=` / `-` | Increase or decrease text size |
| `Ctrl+Alt+S` | Save the current transcript |

## Audio and languages

Capture all system audio, a single app or your microphone. You can also mix the microphone with system audio. Use headphones when mixing to avoid picking up the speaker output twice.

Select a spoken language or let the recognition model detect it. Automatic detection also sets the source language for translation. The translation language list has 53 entries, but each recognition and translation model has its own language coverage. A language appearing in a menu does not mean every model supports it or that every language pair has been tested.

The default context note is written for Japanese-to-Traditional-Chinese subtitles. Edit it to suit your target language when changing language pairs.

For transcription without translation, turn translation off. The interface is available in Traditional Chinese, English and Japanese.

Translations can use recent source/translation pairs, a context note and a glossary. Profiles save these settings and recognition hints alongside the subtitle appearance.

## Transcripts, recording and OBS

Pausing or stopping does not clear the current transcript. When you resume, new lines are appended to it. Search the transcript in the app, or export it as TXT, SRT or WebVTT. The title-bar save button and `Ctrl+Alt+S` save a TXT file in `transcripts`.

Two optional outputs are available in settings:

- **Live subtitle file:** writes the current on-screen subtitle to `transcripts\live.txt`, which OBS can read as a text source.
- **Audio recording:** saves the audio sent to recognition, including silence, as a 16 kHz mono WAV. Each start creates a new recording, at roughly 115 MB per hour. Recording and transcript timestamps share the same starting point.

Automatic transcript saving is optional. You can also choose whether earlier subtitles show the original text, the translation or both.

## Partial results and experimental options

During speech, Sonivra transcribes the audio received so far every 0.8 seconds. These partial results appear dimmed and are replaced when the segment is finalized. They can change as more audio arrives. Only finalized results are added to the transcript, and by default only those results are translated.

Two optional features are off by default:

- **Translate partial results:** translates unfinished speech when the translator is idle. Sonivra skips this work when latency is high or audio is waiting to be processed.
- **Revise the previous translation:** uses the next sentence as context to translate the previous one again. Changes appear on screen and in subsequent exports, but do not rewrite files already saved automatically.

## Known limitations

- Speech segmentation uses an RMS volume threshold. Music can keep a segment open, and very quiet speech can fall below the threshold. Adjust sensitivity on the audio settings page.
- Recognition and translation each use one worker, with queues limited to two segments and one segment respectively. Older segments are dropped if processing falls behind.
- Speaker labels, re-transcribing recent audio, a second subtitle window, an installer and automatic updates are not available.

## Recorded performance

These figures are from an existing release-build test, not a guarantee for other audio, models or hardware. The test captured 75 seconds of Japanese narration from the public-domain LibriVox recording of *Akai rōsoku to ningyo* through WASAPI loopback, using the default model pair on Windows 11 with an RTX 5070 (12 GB) and 32 GB RAM.

| Measurement | Result |
|---|---|
| Segments translated | 21, with none dropped |
| End of speech to completed translation, median / P95 | 606 ms / 628 ms |
| Endpoint wait, last segment | 420 ms |
| Recognition, last segment | 47 ms |
| Translation time to first token, last segment | 35 ms |
| Main process RAM | 65 MiB |
| Recognition worker RAM | 258 MiB |
| Translation worker RAM | 2,469 MiB |
| Total GPU memory in use, reported by NVML | 6,674 MiB while running, about 2,700 MiB before Sonivra started |

RAM figures are process working sets (RSS). The GPU figures include other applications and are not Sonivra-only allocations.

## Build from source

Install stable Rust and the Visual Studio C++ build tools with the Windows SDK, then run:

```powershell
cargo build --release
```

The executable is written to `target/release/Sonivra.exe`. Run `cargo test` for unit tests. The integration tests in `tests/real_models.rs` need actual models and runtimes and are ignored by default.

## Licensing

Sonivra is licensed under [GPL-3.0-only](LICENSE). See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for the statically linked Rust dependencies and their licenses.

Models and inference runtimes are downloaded from their upstream sources when you request them. They are not bundled with the app, and each has its own license. The download catalogs, `catalog.json` and `runtimes.json`, record those licenses. PLaMo models, for example, use the PLaMo Community License.

The Japanese test clips in `tests/audio` come from Google FLEURS under CC BY 4.0. See [the audio attribution](tests/audio/ATTRIBUTION.md).
