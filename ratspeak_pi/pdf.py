"""An article as a small PDF laid out like its web page, to send over Ratspeak.

Only the PDF standard fonts are used (Times for the title and section headings, Helvetica for the text), so
nothing is embedded and a long article stays a few tens of KB, which matters on a mesh. Text outside
Windows-1252 falls back to plain letters.
"""
from __future__ import annotations

import unicodedata
import zlib

PAGE_W, PAGE_H, MARGIN = 595, 842, 50          # A4, in points
TOP, BOTTOM = PAGE_H - MARGIN, MARGIN + 24     # the footer sits below BOTTOM
CONTENT_W = PAGE_W - 2 * MARGIN
BOX_W, GUTTER = 196, 14                        # the facts box floats right, as an infobox does
INK, MUTED, RULE, FILL, EDGE = 0.13, 0.42, 0.66, 0.965, 0.78

# Character widths per 1000 units for ASCII 32-126 (Adobe font metrics); other characters use their base letter.
_HELVETICA = [278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, *[556] * 10, 278, 278,
              584, 584, 584, 556, 1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667,
              778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556,
              278, 556, 556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500,
              334, 260, 334, 584]
_HELVETICA_BOLD = [278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, *[556] * 10, 333,
                   333, 584, 584, 584, 611, 975, 722, 722, 722, 722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722,
                   778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 333, 278, 333, 584, 556, 333, 556, 611,
                   556, 611, 556, 333, 611, 611, 278, 278, 556, 278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556,
                   778, 556, 556, 500, 389, 280, 389, 584]
_TIMES = [250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278, *[500] * 10, 278, 278, 564,
          564, 564, 444, 921, 722, 667, 667, 722, 611, 556, 722, 722, 333, 389, 722, 611, 889, 722, 722, 556, 722, 667,
          556, 611, 722, 722, 944, 722, 722, 611, 333, 278, 333, 469, 500, 333, 444, 500, 444, 500, 444, 333, 500, 500,
          278, 278, 500, 278, 778, 500, 500, 500, 500, 333, 389, 278, 500, 500, 722, 500, 500, 444, 480, 200, 480, 541]
# Windows-1252 punctuation whose width differs from its base letter: dashes, ellipsis, curly quotes, bullet.
_MARKS = "–—…‘’“”•"
_PUNCTUATION = {"Helvetica": [556, 1000, 1000, 222, 222, 333, 333, 350],
                "Helvetica-Bold": [556, 1000, 1000, 278, 278, 500, 500, 350],
                "Times-Roman": [500, 1000, 1000, 333, 333, 444, 444, 350]}
# Font resource name, PostScript name, widths.
FONTS = {"sans": ("F1", "Helvetica", _HELVETICA), "bold": ("F2", "Helvetica-Bold", _HELVETICA_BOLD),
         "italic": ("F3", "Helvetica-Oblique", _HELVETICA), "serif": ("F4", "Times-Roman", _TIMES)}


# Marks Windows-1252 lacks but has a near twin for: primes in coordinates, minus signs, odd spaces and hyphens.
_TWINS = str.maketrans({"\u2032": "'", "\u2033": '"', "\u2212": "-", "\u2010": "-", "\u2011": "-", "\u2009": " ",
                        "\u200a": " ", "\u202f": " ", "\u2007": " ", "\u200b": "", "\u200c": "", "\u200d": "",
                        "\ufeff": "", "\u2060": ""})


def _plain(text: str) -> str:
    """Text the standard fonts can show: Windows-1252 where possible, otherwise the base letters.
    A run of characters with neither (another script) becomes one ellipsis."""
    out = []
    for char in text.translate(_TWINS):
        try:
            char.encode("cp1252")
            out.append(char)
        except UnicodeEncodeError:
            base = "".join(c for c in unicodedata.normalize("NFKD", char) if not unicodedata.combining(c))
            if base and all(ord(c) < 128 for c in base):
                out.append(base)
            elif not out or out[-1] != "…":
                out.append("…")
    return "".join(out)


def _width(text: str, size: float, font: str) -> float:
    _, name, table = FONTS[font]
    total = 0
    for char in text:
        code = ord(char)
        if char in _MARKS:
            total += _PUNCTUATION[name.removesuffix("-Oblique")][_MARKS.index(char)]
            continue
        if not 32 <= code <= 126:
            base = unicodedata.normalize("NFKD", char)[:1]
            code = ord(base) if base and 32 <= ord(base) <= 126 else 110        # an "n" for anything else
        total += table[code - 32]
    return total * size / 1000


def _escape(text: str) -> bytes:
    return text.encode("cp1252").replace(b"\\", b"\\\\").replace(b"(", b"\\(").replace(b")", b"\\)")


def _take(words: list[str], size: float, font: str, width: float) -> tuple[str, list[str]]:
    """The longest line that fits, and the words left over. A word wider than the line is split."""
    line, rest = "", list(words)
    while rest:
        candidate = f"{line} {rest[0]}" if line else rest[0]
        if _width(candidate, size, font) <= width:
            line = candidate
            rest.pop(0)
        elif not line:
            cut = max(1, len(rest[0]) - 1)
            while cut > 1 and _width(rest[0][:cut], size, font) > width:
                cut -= 1
            line, rest[0] = rest[0][:cut], rest[0][cut:]
            break
        else:
            break
    return line, rest


def _lines(text: str, size: float, font: str, width: float, limit: int = 99) -> list[str]:
    words, out = text.split(), []
    while words and len(out) < limit:
        line, words = _take(words, size, font, width)
        out.append(line)
    if words and out:
        out[-1] = out[-1].rstrip(".,;:") + "…"
    return out


class _Pages:
    """Places text top to bottom across A4 pages. A box floating at the right narrows the lines beside it."""

    def __init__(self):
        self.pages, self.ops, self.y = [], [], TOP
        self.float_bottom = None

    def width(self, indent: float = 0) -> float:
        beside = self.float_bottom is not None and self.y > self.float_bottom
        return CONTENT_W - indent - (BOX_W + GUTTER if beside else 0)

    def new_page(self):
        self.pages.append(self.ops)
        self.ops, self.y, self.float_bottom = [], TOP, None

    def room(self, height: float):
        if self.y - height < BOTTOM:
            self.new_page()

    def text(self, x: float, y: float, font: str, size: float, line: str, gray: float = INK):
        self.ops.append(b"BT %.2f g /%s %.1f Tf 1 0 0 1 %.1f %.1f Tm (%s) Tj ET"
                        % (gray, FONTS[font][0].encode(), size, x, y, _escape(line)))

    def rule(self, x1: float, x2: float, y: float):
        self.ops.append(b"q %.2f G 0.6 w %.1f %.1f m %.1f %.1f l S Q" % (RULE, x1, y, x2, y))

    def box(self, x: float, y: float, w: float, h: float):
        self.ops.append(b"q %.3f g %.2f G 0.6 w %.1f %.1f %.1f %.1f re B Q" % (FILL, EDGE, x, y, w, h))

    def paragraph(self, text: str, font: str, size: float, leading: float, indent: float = 0, marker: str = "",
                  gap: float = 6, gray: float = INK):
        words, first = text.split(), True
        while words:
            self.room(leading)
            line, words = _take(words, size, font, self.width(indent))
            self.y -= leading
            if first and marker:
                self.text(MARGIN + indent - _width(marker, size, font) - 5, self.y, font, size, marker, gray)
            self.text(MARGIN + indent, self.y, font, size, line, gray)
            first = False
        self.y -= gap


def _facts(pages: _Pages, title: str, facts: list[tuple[str, str]]):
    """The infobox: a shaded box at the top right with the article's key facts."""
    pad, label_w, size, leading = 7, 66, 8.5, 11
    value_w = BOX_W - 2 * pad - label_w - 6
    heading = _lines(title, 10.5, "bold", BOX_W - 2 * pad, 2)
    rows = [(_lines(label, size, "bold", label_w, 3), _lines(value, size, "sans", value_w, 4)) for label, value in facts]
    height = pad + len(heading) * 13 + 4
    kept = []
    for label, value in rows:
        step = max(len(label), len(value)) * leading + 4
        if pages.y - height - step - pad < BOTTOM + 40:
            break
        kept.append((label, value))
        height += step
    height += pad
    x, top = PAGE_W - MARGIN - BOX_W, pages.y
    pages.box(x, top - height, BOX_W, height)
    y = top - pad
    for line in heading:
        y -= 13
        pages.text(x + (BOX_W - _width(line, 10.5, "bold")) / 2, y, "bold", 10.5, line)
    y -= 4
    for label, value in kept:
        row_top = y
        for n, line in enumerate(label):
            pages.text(x + pad, row_top - leading * (n + 1), "bold", size, line)
        for n, line in enumerate(value):
            pages.text(x + pad + label_w + 6, row_top - leading * (n + 1), "sans", size, line)
        y = row_top - max(len(label), len(value)) * leading - 4
    pages.float_bottom = top - height - 8


def _contents(pages: _Pages, headings: list[tuple[str, str]]):
    """The table of contents box Wikipedia shows before the first section."""
    pad, size, leading = 8, 9.5, 13
    entries, major, minor = [], 0, 0
    for kind, text in headings:
        if kind == "h2":
            major, minor = major + 1, 0
            entries.append((f"{major}", text, 0))
        elif major:
            minor += 1
            entries.append((f"{major}.{minor}", text, 14))
    entries = entries[:24]
    pages.room(pad * 2 + leading * (len(entries) + 1) + 4)
    width = min(pages.width(), max(_width("Contents", 10, "bold"),
                                   *(indent + 26 + _width(text, size, "sans") for _, text, indent in entries)) + 2 * pad)
    height = pad * 2 + leading * (len(entries) + 1) + 4
    top = pages.y
    pages.box(MARGIN, top - height, width, height)
    pages.text(MARGIN + pad, top - pad - 10, "bold", 10, "Contents")
    y = top - pad - leading - 4
    for number, text, indent in entries:
        y -= leading
        line = _lines(text, size, "sans", width - 2 * pad - indent - 26, 1)[0]
        pages.text(MARGIN + pad + indent, y, "sans", size, number, MUTED)
        pages.text(MARGIN + pad + indent + 26, y, "sans", size, line)
    pages.y = top - height - 12


def article(title: str, blocks: list[dict], facts: list[tuple[str, str]], tagline: str, credit: str) -> bytes:
    """A4 pages laid out like the article's web page: the title over a rule, the tagline, a key-facts box,
    contents, section headings over rules, paragraphs and lists, and the credit and page number on every page.
    Each block is {"kind": "p" | "li" | "h2" | "h3" | "h4", "text": str, "depth": int, "marker": str}."""
    heading, title, tagline, credit = title, _plain(title), _plain(tagline), _plain(credit)
    # After _plain: a heading of only zero-width characters is empty, and an empty line has nothing to place.
    blocks = [block for block in (dict(b, text=_plain(b["text"])) for b in blocks) if block["text"].strip()]
    facts = [(_plain(label), _plain(value)) for label, value in facts]
    pages = _Pages()
    for line in _lines(title, 24, "serif", CONTENT_W, 3):
        pages.y -= 28
        pages.text(MARGIN, pages.y, "serif", 24, line)
    pages.y -= 7
    pages.rule(MARGIN, PAGE_W - MARGIN, pages.y)
    pages.y -= 14
    pages.text(MARGIN, pages.y, "italic", 9, tagline, MUTED)
    pages.y -= 14
    if facts:
        _facts(pages, title, facts[:16])
    headings = [(b["kind"], b["text"]) for b in blocks if b["kind"] in ("h2", "h3")]
    contents = sum(kind == "h2" for kind, _ in headings) >= 3
    for block in blocks:
        kind, text = block["kind"], block["text"]
        if kind == "h2":
            if contents:
                _contents(pages, headings)
                contents = False
            pages.room(64)
            pages.y -= 26
            pages.text(MARGIN, pages.y, "serif", 18, _lines(text, 18, "serif", pages.width(), 1)[0])
            pages.y -= 5
            pages.rule(MARGIN, MARGIN + pages.width(), pages.y)
            pages.y -= 4
        elif kind in ("h3", "h4", "h5", "h6"):
            size = 11.5 if kind == "h3" else 10.5
            pages.room(48)
            pages.y -= 8
            pages.paragraph(text, "bold", size, 15, gap=1)
        elif kind == "li":
            indent = 16 * min(block.get("depth") or 1, 3)
            pages.paragraph(text, "sans", 10, 14, indent, block.get("marker") or "•", gap=3)
        else:
            pages.paragraph(text, "sans", 10, 14.5, gap=7)
    pages.new_page()
    pages = [page for page in pages.pages if page]

    objects = [b"<< /Type /Catalog /Pages 2 0 R >>", None,
               *(b"<< /Type /Font /Subtype /Type1 /BaseFont /%s /Encoding /WinAnsiEncoding >>" % name.encode()
                 for _, name, _ in FONTS.values()),
               # Text strings are PDFDocEncoding or UTF-16; Windows-1252 bytes would show the wrong dashes and quotes.
               b"<< /Title <" + ("\ufeff" + heading[:200]).encode("utf-16-be").hex().encode() + b"> /Producer (Ratspeak Agent) >>"]
    fonts = b" ".join(b"/%s %d 0 R" % (key.encode(), n) for n, (key, _, _) in enumerate(FONTS.values(), 3))
    kids = []
    for number, ops in enumerate(pages, 1):
        footer = f"Page {number} of {len(pages)}"
        tail = [b"BT %.2f g /F1 7.5 Tf 1 0 0 1 %d %d Tm (%s) Tj ET" % (MUTED, MARGIN, MARGIN, _escape(credit)),
                b"BT %.2f g /F1 7.5 Tf 1 0 0 1 %.1f %d Tm (%s) Tj ET"
                % (MUTED, PAGE_W - MARGIN - _width(footer, 7.5, "sans"), MARGIN, _escape(footer))]
        stream = zlib.compress(b"\n".join(ops + tail), 9)
        objects.append(b"<< /Length %d /Filter /FlateDecode >>\nstream\n" % len(stream) + stream + b"\nendstream")
        objects.append(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 %d %d] /Resources << /Font << %s >> >> "
                       b"/Contents %d 0 R >>" % (PAGE_W, PAGE_H, fonts, len(objects)))
        kids.append(len(objects))
    objects[1] = b"<< /Type /Pages /Kids [%s] /Count %d >>" % (b" ".join(b"%d 0 R" % k for k in kids), len(kids))
    info = 3 + len(FONTS)

    out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets = []
    for number, body in enumerate(objects, 1):
        offsets.append(len(out))
        out += b"%d 0 obj\n" % number + body + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objects) + 1)
    out += b"".join(b"%010d 00000 n \n" % offset for offset in offsets)
    out += b"trailer\n<< /Size %d /Root 1 0 R /Info %d 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objects) + 1, info, xref)
    return bytes(out)
