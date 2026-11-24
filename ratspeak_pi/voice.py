"""Voice messages: English speech to text, so a voice note reaches the agent as words.

Whisper (OpenAI, MIT) runs on CTranslate2, the engine translation already uses. Vosk (Alpha Cephei, Apache-2.0)
runs on Pis with about 1 GB, where Whisper would compete with the model for memory. The native helper saves each
voice note as a WAV file; transcription runs in its own short-lived process, like translation.
"""
from __future__ import annotations

import array
import ctypes
import json
import os
import platform
import shutil
import subprocess
import sys
import wave
from pathlib import Path

from . import translate, units

CATALOG = json.loads((Path(__file__).parent / "voice.json").read_text())
ENGINES = CATALOG["engines"]
FOLDER = units.STATE / "voice"
MAX_SECONDS = 120
SPECIAL = "<|"


def choose(desktop: bool, ram_mb: int) -> str | None:
    """The engine for this computer, or None when it has too little memory beside the model."""
    if desktop:
        return "whisper-base"
    if ram_mb < 900:
        return None
    return "vosk" if ram_mb < 1500 else "whisper-tiny"


def installed() -> str | None:
    return next((name for name in ENGINES if (FOLDER / name / "ready").is_file()), None)


def install(engine: str, fetch) -> None:
    """fetch(url, path, sha256) brings one pinned file whole and verified; files land under FOLDER."""
    spec = ENGINES[engine]
    FOLDER.mkdir(parents=True, exist_ok=True)
    if (FOLDER / engine / "ready").is_file():
        return
    staging = FOLDER / (engine + ".new")
    shutil.rmtree(staging, ignore_errors=True)
    staging.mkdir()
    if engine.startswith("whisper"):
        translate.install_engine(fetch)
        for item in spec["files"]:
            fetch(item["url"], FOLDER / item["file"], item["sha256"]).rename(staging / item["path"])
    else:
        arch = {"aarch64": "aarch64", "arm64": "aarch64", "x86_64": "x86_64", "amd64": "x86_64"}.get(platform.machine().lower())
        if arch not in spec["runtime"]:
            raise RuntimeError("Vosk needs 64-bit ARM or x86")
        wheel = spec["runtime"][arch]
        path = fetch(wheel["url"], FOLDER / wheel["file"], wheel["sha256"])
        library = staging / "lib"
        translate.unpack(path, library, strip=False)
        (library / "vosk" / "libvosk.so").rename(staging / "libvosk.so")
        shutil.rmtree(library)
        path.unlink()
        model = spec["files"][0]
        path = fetch(model["url"], FOLDER / model["file"], model["sha256"])
        translate.unpack(path, staging / "model", strip=True)
        path.unlink()
    (staging / "ready").write_text(engine)
    for name in ENGINES:
        shutil.rmtree(FOLDER / name, ignore_errors=True)
    staging.rename(FOLDER / engine)
    translate.prune_engine()


def remove() -> None:
    shutil.rmtree(FOLDER, ignore_errors=True)
    translate.prune_engine()


def remember(message_id, words: str) -> None:
    """Keep what a voice note said, so later turns see it in the conversation history."""
    heard = FOLDER / "heard"
    heard.mkdir(parents=True, exist_ok=True)
    (heard / f"{int(message_id)}.txt").write_text(words)
    for old in sorted(heard.glob("*.txt"), key=lambda f: f.stat().st_mtime)[:-500]:
        old.unlink(missing_ok=True)


def recall(message_id) -> str:
    try:
        return (FOLDER / "heard" / f"{int(message_id)}.txt").read_text()
    except (OSError, TypeError, ValueError):
        return ""


def transcribe(wav: str) -> str | None:
    """The words in a voice note ("" when there are none), or None when transcription failed."""
    engine = installed()
    if not engine:
        return None
    try:
        done = subprocess.run([sys.executable, "-m", "ratspeak_pi.voice", engine, wav], capture_output=True, text=True,
                              timeout=300, cwd=Path(__file__).parent.parent)
    except subprocess.TimeoutExpired:
        return None
    return done.stdout.strip() if done.returncode == 0 else None


# ---- the transcription process ----------------------------------------------------------------------------------

def read(path: str) -> tuple[int, bytes]:
    with wave.open(path, "rb") as clip:
        if clip.getnchannels() != 1 or clip.getsampwidth() != 2:
            raise ValueError("expected 16-bit mono")
        rate = clip.getframerate()
        return rate, clip.readframes(min(clip.getnframes(), rate * MAX_SECONDS))


def mel_filters(np, rate=16000, n_fft=400, n_mels=80):
    """Slaney-scale mel filters, as librosa and Whisper build them (after faster-whisper, MIT)."""
    fft_freqs = np.fft.rfftfreq(n=n_fft, d=1.0 / rate)
    mels = np.linspace(0.0, 45.245640471924965, n_mels + 2)
    freqs = 200.0 / 3 * mels
    log_t = mels >= 15.0
    freqs[log_t] = 1000.0 * np.exp(np.log(6.4) / 27.0 * (mels[log_t] - 15.0))
    ramps = np.subtract.outer(freqs, fft_freqs)
    fdiff = np.diff(freqs)
    weights = np.maximum(0, np.minimum(-ramps[:-2] / fdiff[:-1][:, None], ramps[2:] / fdiff[1:][:, None]))
    return weights * (2.0 / (freqs[2:n_mels + 2] - freqs[:n_mels]))[:, None]


def log_mel(np, audio, filters):
    """Whisper's input: 80 log-mel bands over 30 seconds of 16 kHz audio (3000 frames)."""
    audio = np.pad(audio[:480_000], (0, 480_000 - min(len(audio), 480_000)))
    padded = np.pad(audio, 200, mode="reflect")
    frames = np.lib.stride_tricks.sliding_window_view(padded, 400)[::160]
    power = np.abs(np.fft.rfft(frames * np.hanning(401)[:-1], axis=1)) ** 2
    spec = np.log10(np.maximum(filters @ power.T[:, :-1], 1e-10))
    spec = np.maximum(spec, spec.max() - 8.0)
    return ((spec + 4.0) / 4.0).astype(np.float32)


def pieces(np, audio, rate=16000):
    """Up to 30-second pieces, cut at the quietest tenth of a second between 20 and 30 seconds."""
    out, start, step = [], 0, rate // 10
    while len(audio) - start > 30 * rate:
        window = audio[start + 20 * rate:start + 30 * rate]
        energy = (window[: len(window) // step * step].reshape(-1, step) ** 2).sum(axis=1)
        cut = start + 20 * rate + int(energy.argmin()) * step
        out.append(audio[start:cut])
        start = cut
    out.append(audio[start:])
    return out


def text_of(tokens: list[str]) -> str:
    """Whisper's byte-level BPE tokens as text (GPT-2's byte-to-character table, reversed)."""
    printable = list(range(33, 127)) + list(range(161, 173)) + list(range(174, 256))
    table, extra = {}, 0
    for byte in range(256):
        if byte in printable:
            table[chr(byte)] = byte
        else:
            table[chr(256 + extra)] = byte
            extra += 1
    data = bytes(table[c] for token in tokens if not token.startswith(SPECIAL) for c in token if c in table)
    return data.decode("utf-8", "replace").strip()


def whisper(engine: str, rate: int, data: bytes) -> str:
    sys.path.insert(0, str(translate.ENGINE))
    import ctranslate2
    import numpy as np
    audio = np.frombuffer(data, dtype="<i2").astype(np.float32) / 32768.0
    if rate != 16000:
        audio = np.interp(np.arange(0, len(audio) * 16000 / rate) * rate / 16000, np.arange(len(audio)), audio).astype(np.float32)
    model = ctranslate2.models.Whisper(str(FOLDER / engine), device="cpu", compute_type="int8",
                                       intra_threads=min(4, os.cpu_count() or 1))
    filters, words = mel_filters(np), []
    for piece in pieces(np, audio):
        if len(piece) < 1600:
            continue
        features = ctranslate2.StorageView.from_array(log_mel(np, piece, filters)[None])
        result = model.generate(features, [["<|startoftranscript|>", "<|notimestamps|>"]], beam_size=2,
                                max_length=224, return_no_speech_prob=True)[0]
        if result.no_speech_prob < 0.6:
            words.append(text_of(result.sequences[0]))
    return " ".join(w for w in words if w)


def doubled(data: bytes) -> bytes:
    """8 kHz to 16 kHz by linear interpolation: the English Vosk model hears nothing at 8 kHz."""
    samples = array.array("h", data)
    out = array.array("h", bytes(len(samples) * 4))
    out[0::2] = samples
    out[1::2] = array.array("h", ((a + b) >> 1 for a, b in zip(samples, samples[1:] + samples[-1:])))
    return out.tobytes()


def vosk(engine: str, rate: int, data: bytes) -> str:
    if rate == 8000:
        rate, data = 16000, doubled(data)
    lib = ctypes.CDLL(str(FOLDER / engine / "libvosk.so"))
    lib.vosk_set_log_level(-1)
    lib.vosk_model_new.restype = lib.vosk_recognizer_new.restype = ctypes.c_void_p
    lib.vosk_model_new.argtypes = [ctypes.c_char_p]
    lib.vosk_recognizer_new.argtypes = [ctypes.c_void_p, ctypes.c_float]
    lib.vosk_recognizer_accept_waveform.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
    lib.vosk_recognizer_result.argtypes = lib.vosk_recognizer_final_result.argtypes = [ctypes.c_void_p]
    lib.vosk_recognizer_result.restype = lib.vosk_recognizer_final_result.restype = ctypes.c_char_p
    model = lib.vosk_model_new(str(FOLDER / engine / "model").encode())
    if not model:
        raise RuntimeError("Vosk could not load its model")
    recognizer = lib.vosk_recognizer_new(model, rate)
    words = []
    for at in range(0, len(data), 8000):
        if lib.vosk_recognizer_accept_waveform(recognizer, data[at:at + 8000], len(data[at:at + 8000])):
            words.append(json.loads(lib.vosk_recognizer_result(recognizer))["text"])
    words.append(json.loads(lib.vosk_recognizer_final_result(recognizer))["text"])
    return " ".join(w for w in words if w)


if __name__ == "__main__":
    name, clip = sys.argv[1], sys.argv[2]
    print((whisper if name.startswith("whisper") else vosk)(name, *read(clip)))
