"""Extract selectable PDF text and fit page excerpts to the chat request."""
from __future__ import annotations

import math
import os
import re
import shutil
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path

MAX_PDF_BYTES = 1_000_000  # The entire LXMF message must also fit the transport limit.
MAX_PAGES = 200
MAX_TEXT_CHARS = 512_000
PARSE_TIMEOUT = 15
MAX_OUTPUT_BYTES = MAX_TEXT_CHARS * 4
ANSWER_GUIDANCE = "If the document gives a limit or condition, keep that limit or condition in the answer."
_STOPWORDS = frozenset((
    "a an and are as at attached be can could did do does document file for from had has have how i in is it may me of on or "
    "overview pdf please should so summarize summary that the their them this to use was were what when where which "
    "who why will with would you your"
).split())


class PdfError(ValueError):
    """An attachment error that can be shown to its sender."""


def read_pdf(path: Path) -> tuple[list[tuple[int, str]], bool]:
    """Read bounded page text in a short-lived process."""
    parser = shutil.which("pdftotext")
    if not parser:
        raise PdfError("The PDF reader is unavailable. Install poppler-utils on the agent host.")
    try:
        if path.stat().st_size > MAX_PDF_BYTES:
            raise PdfError("The file is too large to read.")
        with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as diagnostic:
            result = subprocess.run([sys.executable, "-I", str(Path(__file__).resolve()), parser, str(path.resolve())],
                                    stdout=output, stderr=diagnostic, timeout=PARSE_TIMEOUT)
            output.seek(0)
            data = output.read(MAX_OUTPUT_BYTES)
            if result.returncode and len(data) < MAX_OUTPUT_BYTES:
                diagnostic.seek(0)
                if b"incorrect password" in diagnostic.read(4096).lower():
                    raise PdfError("This PDF is password protected.")
                if result.returncode == 3:
                    raise PdfError("This PDF restricts text extraction.")
                raise PdfError("The PDF is invalid or exceeds the extraction limits.")
    except (OSError, subprocess.SubprocessError) as error:
        raise PdfError("The PDF is invalid or exceeds the extraction limits.") from error
    raw_pages = data.decode("utf-8", "replace").removesuffix("\f").split("\f")
    limited = len(raw_pages) > MAX_PAGES or len(data) == MAX_OUTPUT_BYTES
    pages, remaining = [], MAX_TEXT_CHARS
    for number, raw in enumerate(raw_pages[:MAX_PAGES], 1):
        text = " ".join(raw.split())
        if text:
            if len(text) > remaining:
                text = text[:remaining].rstrip()
                limited = True
                if text:
                    pages.append((number, text))
                break
            pages.append((number, text))
            remaining -= len(text)
    if not pages:
        if limited:
            raise PdfError("The pages I could check contained no selectable text.")
        raise PdfError("The PDF contains no selectable text. Scanned pages need OCR.")
    return pages, limited


def _extract(parser: str, path: str) -> None:
    import resource
    # Apply limits before replacing this worker with Poppler. Its stdout is a temporary file.
    resource.setrlimit(resource.RLIMIT_FSIZE, (MAX_OUTPUT_BYTES, MAX_OUTPUT_BYTES))
    if sys.platform == "linux":
        resource.setrlimit(resource.RLIMIT_AS, (192 * 1024**2, 192 * 1024**2))
        resource.setrlimit(resource.RLIMIT_CPU, (10, 12))
    os.execv(parser, [parser, "-l", str(MAX_PAGES + 1), "-enc", "UTF-8", path, "-"])


def _terms(text: str) -> set[str]:
    words = set(re.findall(r"[^\W_]+", text.casefold())) - _STOPWORDS
    return {re.sub(r"(?:ing|ed|s)$", "", word).removesuffix("e") if len(word) > 4 else word for word in words}


def _passages(text: str, size: int):
    start = 0
    while start < len(text):
        end = start + size
        if end >= len(text):
            yield text[start:]
            break
        space = text.rfind(" ", start + size // 2, end)
        if space >= 0:
            end = space
        yield text[start:end]
        # Keep a little overlap so a sentence crossing a boundary is still searchable.
        space = text.find(" ", max(start + 1, end - 120), end)
        start = space + 1 if space >= 0 else end


def attachment_excerpt(path: Path, name: str, question: str, budget: int) -> tuple[str, str]:
    """Return page-labeled text and a notice when only excerpts were supplied."""
    heading = f"Text from PDF {name}:\n"
    if budget < len(heading) + 100:
        raise PdfError("Your question leaves too little room for the PDF text. Try a shorter question.")
    pages, limited = read_pdf(path)
    full = heading + "\n\n".join(f"[Page {number}]\n{text}" for number, text in pages)
    if not limited and len(full) <= budget:
        return full, ""

    heading = f"Selected excerpts from PDF {name}:\n"
    remaining = budget - len(heading)
    # Leave room for two passages when a small context can hold them.
    size = min(840, max(64, remaining // 2 - 16))
    passages = [(number, part) for number, text in pages for part in _passages(text, size)]
    query = _terms(question)
    matches = [_terms(part) & query for _, part in passages]
    frequency = Counter(term for match in matches for term in match)
    if frequency:
        weights = {word: math.log1p(len(passages) / count) for word, count in frequency.items()}
        order = sorted((i for i, match in enumerate(matches) if match),
                       key=lambda i: -sum(weights[word] for word in sorted(matches[i])))
    else:
        # For a broad question, sample across the PDF rather than showing only the first page.
        count = min(len(passages), max(1, remaining // (size + 16)))
        order = [round(i * (len(passages) - 1) / max(1, count - 1)) for i in range(count)]

    selected = []
    for index in order:
        number, part = passages[index]
        # Consecutive windows on a page share text. Use the prompt space for other passages.
        if any(page == number and abs(previous - index) == 1 for previous, page, _ in selected):
            continue
        block = f"[Page {number}]\n{part}"
        if len(block) <= remaining:
            selected.append((index, number, block))
            remaining -= len(block) + 2
    page_numbers = ", ".join(str(number) for number in sorted({number for _, number, _ in selected}))
    notice = f"Used excerpts from {name} (PDF pages {page_numbers})."
    if limited:
        notice += " The document exceeded the PDF extraction limits."
    return heading + "\n\n".join(block for _, _, block in sorted(selected)), notice


if __name__ == "__main__":
    _extract(*sys.argv[1:])
