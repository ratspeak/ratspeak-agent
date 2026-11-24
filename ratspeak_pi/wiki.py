"""/wiki: look a question up in the offline reference libraries on the card, and answer from what is found.

The libraries are Kiwix ZIM files (kiwix.org), each with its own search index, read with libzim. The model does
not decide when to search: the person asks with /wiki, the Pi finds the best article, and the model is given
that text to answer from. A small model is far better at reading a passage than at remembering facts.
"""
from __future__ import annotations

import html
import json
import os
import re
import time
from html.parser import HTMLParser
from pathlib import Path

from . import pdf, units
from .reply import Reply

RESOURCES = json.loads((Path(__file__).parent / "resources.json").read_text())
FOLDER = units.STATE / "resources"
SKIP = {"head", "script", "style", "table", "sup", "figure", "nav", "header", "footer", "math"}
FILLER = set("a an and are can could did do does for from how in is it of on or the to was were what when where which "
             "who whom whose why will would about tell me".split())
VIEWS = units.STATE / "articles.json"      # each person's last lookup, which /view sends as a PDF
VIEW_SECONDS = 86_400
PROMPT = ("Answer the question using only the article below. Reply in plain text, in two to four sentences. "
          "If the article does not answer it, say that it does not.")


class _Text(HTMLParser):
    """The readable paragraphs of an article page, without tables, references, and navigation."""

    def __init__(self):
        super().__init__()
        self.parts, self.skipping = [], 0

    def handle_starttag(self, tag, attrs):
        if tag in SKIP:
            self.skipping += 1
        elif tag in ("p", "li", "h2", "h3") and not self.skipping:
            self.parts.append("\n")

    def handle_endtag(self, tag):
        if tag in SKIP and self.skipping:
            self.skipping -= 1

    def handle_data(self, data):
        if not self.skipping:
            self.parts.append(data)


VOID = {"br", "img", "hr", "meta", "link", "input", "wbr", "area", "base", "col", "embed", "source", "track", "param"}
BLOCKS = {"p", "li", "h1", "h2", "h3", "h4", "h5", "h6", "dd", "dt", "blockquote", "pre"}
# Page furniture that is not the article: reference lists, edit links, navigation boxes, categories.
HIDDEN = {"catlinks", "navbox", "reflist", "references", "mw-references", "hatnote", "noprint", "metadata",
          "mw-editsection", "mw-cite-backlink", "sistersitebox", "shortdescription", "mw-empty-elt", "toc", "printfooter",
          "zim-footer"}
# On a Stack Exchange page only the question, the answers, and their comments are kept.
POSTS = {"s-prose", "comment-copy"}
DROPPED = {"references", "notes", "sources", "citations", "bibliography", "footnotes", "other websites",
           "external links", "further reading"}


class _Page(HTMLParser):
    """An article page as blocks (headings, paragraphs, list items) and its infobox as (label, value) rows."""

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.stack, self.blocks, self.facts, self.lists = [], [], [], []
        self.kind, self.marker, self.words = None, "", []
        self.dropping, self.infobox, self.cell, self.row = False, None, None, {}
        self.cell_at = self.row_at = None          # stack depths that opened the current infobox cell and row
        self.questions, self.posts = False, 0

    def hidden(self):
        if any(hide for _, hide, _ in self.stack):
            return True
        return self.questions and not any(post for _, _, post in self.stack)

    def flush(self):
        text = re.sub(r"\s+", " ", "".join(self.words)).strip()
        kind, self.words, self.kind, marker = self.kind or "p", [], None, self.marker
        if not text:
            return
        if kind in ("h1", "h2"):
            self.dropping = text.lower() in DROPPED
            if kind == "h1":
                return
        if not self.dropping:
            depth = len(self.lists) if kind == "li" else 0
            self.blocks.append({"kind": "p" if kind in ("dd", "dt", "blockquote", "pre") else kind, "text": text,
                                "depth": depth, "marker": marker})

    def handle_starttag(self, tag, attrs):
        attributes = dict(attrs)
        classes = set((attributes.get("class") or "").split())
        if tag == "body" and "question-page" in classes:
            self.questions = True
        if tag in VOID:
            if tag == "br" and not self.hidden():
                (self.row.setdefault(self.cell, []) if self.cell else self.words).append(" ")
            return
        if tag == "table" and "infobox" in classes and self.infobox is None and not self.hidden():
            self.stack.append((tag, False, False))
            self.infobox = len(self.stack)
            return
        hide = (tag in SKIP or bool(classes & HIDDEN) or attributes.get("id") in ("catlinks", "toc")
                or attributes.get("role") == "navigation")
        post = self.questions and bool(classes & POSTS) and not hide
        if post:
            self.flush()
            if "s-prose" in classes:
                self.posts += 1
                heading = "Question" if self.posts == 1 else f"Answer {self.posts - 1}"
                self.blocks.append({"kind": "h2", "text": heading, "depth": 0, "marker": ""})
            else:
                self.kind, self.marker = "li", "\u2013"
        self.stack.append((tag, hide, post))
        if self.infobox is not None:
            if self.hidden():
                return
            if tag == "tr":
                self.row, self.row_at = {}, len(self.stack)
            elif tag in ("th", "td"):
                self.cell, self.cell_at = tag, len(self.stack)
            elif tag in ("li", "div", "p") and self.cell and "".join(self.row.get(self.cell, [])).strip():
                self.row[self.cell].append(", " if tag == "li" else " ")
            return
        if self.hidden():
            return
        if tag in ("ul", "ol"):
            self.flush()
            self.lists.append(0 if tag == "ol" else None)
        elif tag in BLOCKS:
            self.flush()
            self.kind, self.marker = tag, ""
            if tag == "li" and self.lists and self.lists[-1] is not None:
                self.lists[-1] += 1
                self.marker = f"{self.lists[-1]}."

    def handle_endtag(self, tag):
        if not any(entry[0] == tag for entry in self.stack):
            return
        while self.stack:
            open_tag, _, post = self.stack.pop()
            if post:
                self.flush()
            if open_tag == tag:
                break
        if self.infobox is not None:
            # A nested table (a location map) closes its own rows and cells, not the infobox's.
            if self.cell and len(self.stack) < self.cell_at:
                self.cell = None
            if self.row_at and len(self.stack) < self.row_at:
                label, value = (re.sub(r"\s+", " ", "".join(self.row.get(k, []))).strip() for k in ("th", "td"))
                if label and value and len(value) <= 200:
                    self.facts.append((label, value))
                self.row_at = None
            if len(self.stack) < self.infobox:
                self.infobox = None
            return
        if tag in BLOCKS:
            self.flush()
        elif tag in ("ul", "ol") and not self.hidden() and self.lists:
            self.flush()
            self.lists.pop()

    def handle_data(self, data):
        if self.hidden():
            return
        if self.infobox is not None:
            if self.cell:
                self.row.setdefault(self.cell, []).append(data)
            return
        self.words.append(data)


def structure(page: str) -> tuple[list[dict], list[tuple[str, str]]]:
    """The article's blocks and key facts, for its PDF."""
    parser = _Page()
    parser.feed(page)
    parser.flush()
    return parser.blocks, parser.facts


def text_of(page: str) -> str:
    parser = _Text()
    parser.feed(page)
    text = html.unescape("".join(parser.parts))
    text = re.sub(r"\[[^\]]{0,12}\]", "", text)                      # [1], [edit], [change]
    lines = [re.sub(r"\s+", " ", line).strip() for line in text.split("\n")]
    return "\n".join(line for line in lines if len(line) > 1)


COMMANDS = {library["command"]: name for name, library in RESOURCES.items()}


def installed() -> dict[str, Path]:
    """The libraries on this agent, by name, in catalog order."""
    return {name: FOLDER / library["file"] for name, library in RESOURCES.items() if (FOLDER / library["file"]).exists()}


def result_paths(search, count):
    # Debian's reader returns paths; libzim 4 returns records for full-text searches.
    results = search.get_results(0, count) if hasattr(search, "get_results") else search.getResults(0, count)
    return [entry if isinstance(entry, str) else entry.path for entry in results]


def lookup(question: str, name: str = "wikipedia") -> tuple[dict, str, str, str, str] | None:
    """(library, article title, article text, article page, article path) for the best match in one library, or None."""
    from libzim.reader import Archive
    from libzim.search import Query, Searcher
    from libzim.suggestion import SuggestionSearcher

    words = re.findall(r"[\w'-]+", question.lower())
    asks = question.rstrip().endswith("?") or len(words) > 4 or (words and words[0] in FILLER)
    terms = [word for word in words if word not in FILLER] or words
    stem = lambda word: word[:-1] if len(word) > 3 and word.endswith("s") else word
    wanted = {stem(word) for word in terms}
    for path in [installed()[name]] if name in installed() else []:
        archive = Archive(str(path))
        found = result_paths(SuggestionSearcher(archive).suggest(" ".join(terms)), 3)
        if (asks or not found) and archive.has_fulltext_index:
            found += result_paths(Searcher(archive).search(Query().set_query(" ".join(terms))), 8)

        def fit(entry_path):
            """How much of the article's title the question names: "Bee" beats "Africanized bee" for "bees"."""
            title = [stem(word) for word in re.findall(r"[\w'-]+", archive.get_entry_by_path(entry_path).title.lower())]
            return sum(word in wanted for word in title) / len(title) if title else 0

        found.sort(key=fit, reverse=True)       # stable: the search's own order breaks ties
        for entry_path in found[:3]:
            entry = archive.get_entry_by_path(entry_path)
            item = entry.get_item()
            if not item.mimetype.startswith("text/html"):
                continue
            page = bytes(item.content).decode("utf-8", "replace")
            text = text_of(page)
            if len(text) > 80:
                return RESOURCES[name], entry.title, text, page, entry_path
    return None


# Question words and the words articles use for the same thing.
SENSES = {"tall": "height elevation high metres feet", "high": "height elevation metres feet", "big": "area size population",
          "large": "area size population", "old": "born founded built age", "when": "year born founded date",
          "where": "located location lies", "far": "distance km miles", "deep": "depth deepest", "long": "length km miles",
          "many": "number population", "who": "born known", "hot": "temperature climate", "cold": "temperature climate"}


def quote(text: str, question: str, title: str = "", limit: int = 700) -> str:
    """The article's own words: its opening, plus the paragraph that best answers the question when that is
    elsewhere. Used instead of a model's summary when the model is too small to trust."""
    paragraphs = [p for p in text.split("\n") if len(p) > 40]
    if not paragraphs:
        return text[:limit]
    subject = set(re.findall(r"[\w'-]+", title.lower()))
    asked = [w for w in re.findall(r"[\w'-]+", question.lower()) if w not in subject and len(w) > 2]
    wanted = {w for w in asked if w not in FILLER} | {s for w in asked for s in SENSES.get(w, "").split()}
    score = lambda p: len(wanted & set(re.findall(r"[\w'-]+", p.lower())))
    best = max(paragraphs[1:], key=score, default=None)
    # A second paragraph only when it answers better than the opening does.
    chosen = [paragraphs[0]] + ([best] if best and score(best) > score(paragraphs[0]) else [])
    reply = "\n\n".join(chosen)
    return reply if len(reply) <= limit else reply[:limit].rsplit(" ", 1)[0] + "…"


def attach(library: dict, title: str, page: str, outbox: Path) -> list[dict]:
    """The whole article as a PDF laid out like its page, credited to its library and licence."""
    blocks, facts = structure(page)
    if blocks and blocks[0]["text"] == title:        # the page repeats its own title
        blocks.pop(0)
    tagline = f"From {library['name']}, the free encyclopedia" if "Wikipedia" in library["name"] else f"From {library['name']}"
    data = pdf.article(title, blocks, facts, tagline, f"Text from {library['name']}, {library['license']}.")
    if len(data) > 900_000:
        return []
    outbox.mkdir(parents=True, exist_ok=True)
    name = re.sub(r"[^\w .,'()-]", "", title)[:80].strip() or "Article"
    path = outbox / f"{name} {os.urandom(4).hex()}.pdf"
    path.write_bytes(data)
    return [{"path": str(path), "name": f"{name}.pdf", "kind": "file"}]


def remember(peer: str, name: str, path: str, title: str) -> None:
    """Note what this person looked up last; a newer lookup replaces it."""
    views = {}
    try:
        views = json.loads(VIEWS.read_text())
    except (OSError, ValueError):
        pass
    views = views if isinstance(views, dict) else {}
    views[peer] = {"library": name, "path": path, "title": title, "at": time.time()}
    kept = sorted(views.items(), key=lambda item: item[1].get("at", 0) if isinstance(item[1], dict) else 0)[-200:]
    VIEWS.parent.mkdir(parents=True, exist_ok=True)
    pending = VIEWS.with_name(VIEWS.name + ".tmp")
    pending.write_text(json.dumps(dict(kept)))
    pending.replace(VIEWS)


def page_at(name: str, path: str) -> tuple[str, str] | None:
    """(title, page) of one article in an installed library, or None when it is gone."""
    from libzim.reader import Archive
    archive = Archive(str(installed()[name]))
    if not archive.has_entry_by_path(path):
        return None
    entry = archive.get_entry_by_path(path)
    return entry.title, bytes(entry.get_item().content).decode("utf-8", "replace")


def view(peer: str, outbox: Path):
    """/view: the article this person looked up last, as a PDF built from the library now. A lookup more than a
    day old, a removed library, or no lookup at all gets a plain answer saying what to do instead."""
    try:
        record = json.loads(VIEWS.read_text()).get(peer)
    except (OSError, ValueError, AttributeError):
        record = None
    examples = [RESOURCES[name]["example"] for name in installed()]
    if not isinstance(record, dict) or not isinstance(record.get("at"), (int, float)) or record.get("library") not in RESOURCES:
        if not examples:
            return "No libraries are installed on this agent, so there is nothing to show."
        return f"Nothing to show yet. Look something up first, such as {examples[0]}, then send /view."
    title, name = record.get("title", "That article"), record["library"]
    if time.time() - record["at"] > VIEW_SECONDS:
        return f'"{title}" was looked up more than a day ago. Look it up again, then send /view.'
    if name not in installed():
        return f"{RESOURCES[name]['label']} is no longer installed on this agent."
    try:
        found = page_at(name, str(record.get("path", "")))
    except (ImportError, RuntimeError, OSError, ValueError) as error:
        return f"The library could not be read: {str(error)[:120]}"
    if not found:
        return f'"{title}" is no longer in {RESOURCES[name]["name"]}.'
    title, page = found
    files = attach(RESOURCES[name], title, page, outbox)
    if not files:
        return f'"{title}" is too long to send as a PDF.'
    return Reply(f'"{title}" from {RESOURCES[name]["name"]}.', files)


def answer(question: str, ask, limit: int, small: bool = False, name: str = "wikipedia", peer: str | None = None) -> str:
    """The reply to `/wiki question` and the other library commands. It offers the whole article through /view.
    `ask(system, user)` runs the model and may raise RuntimeError. A small model's summary is not trusted:
    the article is quoted instead."""
    library = RESOURCES[name]
    if not question.strip():
        return f"Ask about something, such as {library['example']}"
    if name not in installed():
        return f"{library['label']} is not installed on this agent. The owner can add it in Ratspeak Agent."
    try:
        found = lookup(question.strip(), name)
    except (ImportError, RuntimeError, OSError, ValueError) as error:
        return f"The library could not be searched: {str(error)[:120]}"
    if not found:
        return "I found nothing about that. Try fewer words, or a different name."
    library, title, full, page, path = found
    note = ""
    if peer:
        try:
            remember(peer, name, path, title)
            note = f"\n\nSend /view for the full {library.get('item', 'article')}."
        except OSError:
            pass
    if small:
        return f"From {library['name']}, \"{title}\":\n{quote(full, question, title)}{note}"
    text = full[:limit].rsplit("\n", 1)[0] if len(full) > limit else full
    source = library["name"]
    # A bare topic ("Mount Everest") is a request for the gist, not a question to answer.
    words = question.strip().split()
    asked = question.strip() if question.strip().endswith("?") or len(words) > 4 or words[0].lower() in FILLER \
        else f"What is {title}? Give the most important facts."
    try:
        reply = ask(PROMPT, f"Article: {title}\n{text}\n\nQuestion: {asked}").strip()
    except RuntimeError:
        reply = ""
    # If the model is down the article's opening is still an answer.
    return f"From {source}, \"{title}\":\n{reply or text[:600].rsplit(' ', 1)[0] + '…'}{note}"
