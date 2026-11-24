"""/translate: Argos Translate packs (CTranslate2 models with SentencePiece) when they cover the request, else the model.

Each language is a pair of packs, to and from English; two other languages go through English. The engine runs in its
own short-lived process, so the agent holds no translation model in memory between requests.
"""
from __future__ import annotations

import json
import os
import platform
import re
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

from . import units

CATALOG = json.loads((Path(__file__).parent / "translations.json").read_text())
LANGUAGES = CATALOG["languages"]
FOLDER = units.STATE / "translate"
ENGINE = FOLDER / "engine"
LIMIT = 1500
SCRIPTS = (("ar", "؀-ۿ"), ("zh", "㐀-䶿一-鿿"), ("hi", "ऀ-ॿ"), ("uk", "Ѐ-ӿ"))
WORDS = {
    "en": set("the and is are you your to of what where when how this that it i we my have do not for with".split()),
    "es": set("el la los las de que y es está están dónde donde por para una un no mi tu con qué cómo hay".split()),
    "fr": set("le la les de et est que où vous je une un des pas mon ton avec qui quoi il nous".split()),
    "pt": set("o a os as de e é que onde você um uma não meu seu com está tem para em".split()),
    "de": set("der die das und ist nicht ich du wo wie was ein eine zu mit auf für sie wir".split()),
    "it": set("il lo gli di che è non sono ho per con dove come questo questa mi ti ci della".split()),
}
ACCENTS = {"es": "ñ¿¡", "pt": "ãõç", "fr": "êœç", "de": "äöüß", "it": "èìò"}
# Languages the model may know but no pack covers: a first word among these names the target language.
OTHER = set("""russian korean dutch polish turkish greek hebrew swedish norwegian danish finnish czech slovak
romanian hungarian bulgarian serbian croatian slovenian vietnamese thai indonesian malay persian farsi urdu bengali
punjabi tamil telugu marathi gujarati swahili tagalog filipino catalan basque galician irish welsh latin esperanto
icelandic estonian latvian lithuanian albanian armenian georgian kazakh uzbek mongolian nepali sinhala burmese khmer
lao amharic somali yoruba hausa igbo zulu xhosa afrikaans haitian""".split())
EXAMPLE = "Example: /translate Spanish Where is the station? Without a language, the text is translated into English."


def runtime() -> list[dict]:
    """The engine's pinned wheels for this machine, or [] where none exist."""
    arch = {"aarch64": "aarch64", "arm64": "aarch64", "x86_64": "x86_64", "amd64": "x86_64"}.get(platform.machine().lower())
    return CATALOG["runtime"][arch] if arch and sys.version_info[:2] == (3, 13) else []


def folder(pack: dict) -> Path:
    return FOLDER / pack["file"].removesuffix(".argosmodel")


def ready(feature: str) -> bool:
    return (ENGINE / "ready").is_file() and all((folder(p) / "model/model.bin").is_file() for p in LANGUAGES[feature]["packs"])


def directions() -> set[tuple[str, str]]:
    """Every (from, to) pair a downloaded pack can translate now."""
    if not (ENGINE / "ready").is_file():
        return set()
    return {(p["from"], p["to"]) for lang in LANGUAGES.values() for p in lang["packs"] if (folder(p) / "model/model.bin").is_file()}


def installed() -> dict[str, str]:
    """Language code -> name for every language with a downloaded pack, in catalog order."""
    have = {code for pair in directions() for code in pair} - {"en"}
    return {lang["code"]: lang["label"] for lang in LANGUAGES.values() if lang["code"] in have}


def name_of(code: str) -> str:
    return "English" if code == "en" else next(lang["label"] for lang in LANGUAGES.values() if lang["code"] == code)


def unpack(archive: Path, target: Path, strip: bool) -> None:
    """Extract a wheel or a pack. Pack folders drop their top folder and the unused sentence splitter."""
    with zipfile.ZipFile(archive) as bundle:
        for member in bundle.infolist():
            parts = Path(member.filename).parts[1 if strip else 0:]
            if member.is_dir() or not parts or (strip and parts[0] == "stanza"):
                continue
            if member.filename.startswith("/") or ".." in parts:
                raise RuntimeError(f"Unsafe path in {archive.name}")
            path = target.joinpath(*parts)
            path.parent.mkdir(parents=True, exist_ok=True)
            with bundle.open(member) as source, path.open("wb") as out:
                shutil.copyfileobj(source, out)


def install_engine(fetch) -> None:
    """CTranslate2 and its wheels, shared by translation and Whisper voice transcription."""
    wheels = runtime()
    if not wheels:
        raise RuntimeError("this needs Python 3.13 on 64-bit ARM or x86")
    FOLDER.mkdir(parents=True, exist_ok=True)
    stamp = "\n".join(w["sha256"] for w in wheels)
    if not (ENGINE / "ready").is_file() or (ENGINE / "ready").read_text() != stamp:
        staging = FOLDER / "engine.new"
        shutil.rmtree(staging, ignore_errors=True)
        for wheel in wheels:
            path = fetch(wheel["url"], FOLDER / wheel["file"], wheel["sha256"])
            unpack(path, staging, strip=False)
            path.unlink()
        (staging / "ready").write_text(stamp)
        shutil.rmtree(ENGINE, ignore_errors=True)
        staging.rename(ENGINE)


def prune_engine() -> None:
    """Remove the engine once no translation language or Whisper transcription uses it."""
    from . import voice
    if not any(ready(name) for name in LANGUAGES) and not (voice.installed() or "").startswith("whisper"):
        shutil.rmtree(ENGINE, ignore_errors=True)


def install(feature: str, fetch) -> None:
    """fetch(url, path, sha256) brings one pinned file whole and verified."""
    install_engine(fetch)
    for pack in LANGUAGES[feature]["packs"]:
        target = folder(pack)
        if (target / "model/model.bin").is_file():
            continue
        path = fetch(pack["url"], FOLDER / pack["file"], pack["sha256"])
        staging = target.with_name(target.name + ".new")
        shutil.rmtree(staging, ignore_errors=True)
        unpack(path, staging, strip=True)
        shutil.rmtree(target, ignore_errors=True)
        staging.rename(target)
        path.unlink()


def remove(feature: str) -> None:
    for pack in LANGUAGES[feature]["packs"]:
        shutil.rmtree(folder(pack), ignore_errors=True)
        for leftover in (FOLDER / pack["file"], FOLDER / (pack["file"] + ".part")):
            leftover.unlink(missing_ok=True)
    prune_engine()


def join_names(names: list[str]) -> str:
    return names[0] if len(names) == 1 else " and ".join(names) if len(names) == 2 else ", ".join(names[:-1]) + ", and " + names[-1]


def code_for(word: str) -> str | None:
    word = word.strip().lower()
    # Names only: a bare code such as "it" or "hi" is also an ordinary first word.
    if word in ("english", "inglés", "ingles", "anglais", "inglês"):
        return "en"
    return next((lang["code"] for lang in LANGUAGES.values() if word in lang["names"]), None)


def detect(text: str) -> str:
    """The language a message is in, by its script, then common words and accents; "" when unsure."""
    if re.search("[\u3040-\u30ff]", text):   # kana: Chinese never uses it
        return "ja"
    for code, letters in SCRIPTS:
        if len(re.findall(f"[{letters}]", text)) * 3 >= len(re.sub(r"\W", "", text)):
            return "ru" if code == "uk" and not re.search("[іїєґІЇЄҐ]", text) else code
    lower = text.lower()
    words = re.findall(r"[^\W\d_]+", lower)
    scores = {code: sum(w in WORDS[code] for w in words) + 2 * sum(lower.count(a) for a in ACCENTS.get(code, ""))
              for code in WORDS}
    best = max(scores, key=lambda code: (scores[code], code == "en"))
    return best if scores[best] else ""


def route(source: str, target: str) -> list[tuple[str, str]]:
    return [(source, target)] if "en" in (source, target) else [(source, "en"), ("en", target)]


def reply(rest: str, model) -> str:
    """Answer /translate. `model(instruction, text)` asks the agent's model when no downloaded pack covers it."""
    word, _, tail = rest.partition(" ")
    named = bool(tail.strip()) and (code_for(word) is not None or word.lower() in OTHER)
    text = (tail if named else rest).strip()[:LIMIT]
    if not text or not tail.strip() and (code_for(word) or word.lower() in OTHER):
        return EXAMPLE
    source, have = detect(text), directions()
    if source == "ru" and not re.search("[ыэъёЫЭЪЁ]", text) and ("uk", "en") in have:
        source = "uk"   # Cyrillic with no letter only Russian uses
    if named:
        target = code_for(word)
        target_name = name_of(target) if target else word.capitalize()
    elif source == "en":
        # English with no language named: the one language this agent can translate into, if there is only one.
        out = [code for code in installed() if ("en", code) in have]
        if len(out) != 1:
            return "That is already English. Name a language first, such as /translate Spanish Where is the station?"
        target, target_name = out[0], name_of(out[0])
    else:
        target, target_name = "en", "English"
    if source and source == target:
        return f"That already reads as {target_name}."
    if source and target and all(hop in have for hop in route(source, target)):
        try:
            done = subprocess.run([sys.executable, "-m", "ratspeak_pi.translate", source, target], input=text,
                                  capture_output=True, text=True, timeout=240, cwd=Path(__file__).parent.parent)
        except subprocess.TimeoutExpired:
            return "The translation took too long. Try a shorter message."
        if done.returncode == 0 and done.stdout.strip():
            return done.stdout.strip()
    return model(f"Translate the user's text into {target_name}. Reply with the translation only, "
                 "with no notes and no quotation marks.", text)


def sentences(text: str) -> list[str]:
    return [s.strip() for s in re.split(r"(?<=[.!?])\s+|(?<=[。！？])|\n+", text) if s.strip()][:40]


def run(source: str, target: str, text: str) -> str:
    sys.path.insert(0, str(ENGINE))
    import ctranslate2
    import sentencepiece
    for start, end in route(source, target):
        pack = next(p for lang in LANGUAGES.values() for p in lang["packs"] if (p["from"], p["to"]) == (start, end))
        home = folder(pack)
        pieces = sentencepiece.SentencePieceProcessor(model_file=str(home / "sentencepiece.model"))
        model = ctranslate2.Translator(str(home / "model"), device="cpu", compute_type="auto",
                                       inter_threads=1, intra_threads=min(4, os.cpu_count() or 1))
        results = model.translate_batch([pieces.encode(s, out_type=str) for s in sentences(text)], beam_size=2,
                                        replace_unknowns=True, length_penalty=0.2, max_decoding_length=256)
        parts = [pieces.decode_pieces(r.hypotheses[0]).replace("▁", " ").strip() for r in results]
        text = ("" if end in ("zh", "ja") else " ").join(parts)
    return text


if __name__ == "__main__":
    print(run(sys.argv[1], sys.argv[2], sys.stdin.read()))
