"""The docs as plain HTML pages, for a browser that can't run the docs' program (no WebGPU, as on
many phones) and for whoever reads a page without scripts: each Markdown page of the export
(`wrela doc --export`) as `<path>.html` beside it, styled as the docs are, with the same anchors,
and linked to the pages before and after it. `pages.js` sends such a browser from an address of
the docs (`/#std/arena:arena-insert`) to its page (`/std/arena.html#arena-insert`).

The Markdown is the export's: headings, paragraphs, lists, fenced code, tables, quotes and rules,
with code, bold, italics and links inline (docsite/markdown.wrela reads the same)."""

import html
import json
import re
import shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# The docs' fonts, served beside the pages: Manrope for text, JetBrains Mono for code.
FONTS = [("ui/brand/fonts/Manrope-Medium.ttf", "Manrope"), ("ui/fonts/JetBrainsMono-Regular-Latin1.ttf", "JetBrains Mono")]

# The docs' colours (docsite/theme.wrela): the reader's scheme, dark unless it asks for light.
CSS = """@font-face { font-family: Manrope; src: url(Manrope-Medium.ttf); font-display: swap; }
@font-face { font-family: "JetBrains Mono"; src: url(JetBrainsMono-Regular-Latin1.ttf); font-display: swap; }
:root {
  --page: #15171B; --surface: #1C1F24; --rule: #262A31; --text: #C9CED5; --strong: #F1F3F5;
  --quiet: #8B929C; --field: #A9BCD0; color-scheme: dark;
}
@media (prefers-color-scheme: light) {
  :root {
    --page: #F8F9FB; --surface: #EEF0F3; --rule: #DCE0E5; --text: #2E333B; --strong: #0D1015;
    --quiet: #5B636E; --field: #2D5B88; color-scheme: light;
  }
}
* { box-sizing: border-box; }
html { -webkit-text-size-adjust: 100%; text-size-adjust: 100%; }
body {
  margin: 0; background: var(--page); color: var(--text);
  font: 16px/1.65 Manrope, system-ui, -apple-system, sans-serif; letter-spacing: -0.005em;
}
header {
  position: sticky; top: 0; z-index: 1; display: flex; align-items: center; gap: 12px;
  padding: 14px max(16px, env(safe-area-inset-left)); background: #000; color: #F4F5F7;
}
header a { color: inherit; text-decoration: none; }
header .mark { font-weight: 700; letter-spacing: 0.12em; font-size: 17px; }
header .docs { color: #8B929C; font-size: 15px; }
header .live { margin-left: auto; font-size: 14px; color: #A9BCD0; }
main, footer { max-width: 42rem; margin: 0 auto; padding: 0 20px; }
main { padding-top: 28px; }
.group { color: var(--quiet); font-size: 14px; margin: 0 0 6px; }
h1, h2, h3, h4 { color: var(--strong); line-height: 1.25; font-weight: 700; scroll-margin-top: 72px; }
h1 { font-size: 30px; margin: 0 0 14px; letter-spacing: -0.01em; }
h1 + p { font-size: 18px; line-height: 1.6; }
h2 { font-size: 22px; margin: 40px 0 10px; }
h3 { font-size: 18px; margin: 28px 0 8px; }
h4 { font-size: 16px; margin: 22px 0 6px; }
h1 a.anchor, h2 a.anchor, h3 a.anchor, h4 a.anchor { color: inherit; text-decoration: none; }
p, ul, ol, blockquote, table { margin: 0 0 14px; }
ul, ol { padding-left: 22px; }
li { margin: 4px 0; }
a { color: var(--strong); text-decoration-color: var(--field); text-underline-offset: 3px; }
strong { color: var(--strong); }
code {
  font: 0.88em "JetBrains Mono", ui-monospace, Menlo, monospace; background: var(--surface);
  padding: 1px 4px; border-radius: 3px; overflow-wrap: anywhere;
}
pre {
  background: var(--surface); border-radius: 6px; padding: 14px; overflow-x: auto;
  font-size: 13.5px; line-height: 1.55; margin: 0 0 16px; -webkit-overflow-scrolling: touch;
}
pre code { background: none; padding: 0; font-size: 1em; overflow-wrap: normal; }
p.meta { font-size: 13px; color: var(--quiet); margin-top: -4px; }
p.meta code { background: none; padding: 0; color: var(--field); }
blockquote { margin-left: 0; padding-left: 14px; border-left: 3px solid var(--field); color: var(--quiet); }
hr { border: 0; border-top: 1px solid var(--rule); margin: 28px 0; }
.table { overflow-x: auto; margin: 0 0 16px; }
table { border-collapse: collapse; font-size: 14px; min-width: 100%; }
th, td { text-align: left; padding: 8px 10px; border-bottom: 1px solid var(--rule); vertical-align: top; }
th { color: var(--strong); }
footer { padding-top: 12px; padding-bottom: 48px; }
footer nav { display: flex; justify-content: space-between; gap: 16px; border-top: 1px solid var(--rule); padding-top: 18px; }
footer nav a { text-decoration: none; display: block; max-width: 48%; }
footer nav a span { display: block; color: var(--quiet); font-size: 13px; }
footer nav a.next { text-align: right; margin-left: auto; }
footer .md { display: inline-block; margin-top: 20px; font-size: 14px; color: var(--field); }
"""

# Sends a browser that can't run the docs' program (the browser host shows the page's own text:
# `text-shown`) from an address of the docs to its page as HTML.
SCRIPT = """// When the docs' program can't run here, the page an address names opens as plain HTML.
new MutationObserver(function () {
  if (!document.documentElement.classList.contains("text-shown")) return;
  var h = location.hash.slice(1), k = h.indexOf(":");
  var page = k < 0 ? h : h.slice(0, k), place = k < 0 ? "" : h.slice(k + 1);
  if (!/^[A-Za-z0-9_./-]+$/.test(page) || page.indexOf("..") >= 0) return;
  location.replace("/" + page + ".html" + (place && /^[A-Za-z0-9_.-]+$/.test(place) ? "#" + place : ""));
}).observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });
"""


def slug(heading):
    """A heading's anchor, as the export makes it (compiler/cli/src/export.rs `slug`)."""
    out = ""
    for c in heading.lower():
        if c.isalnum():
            out += c
        elif c in " -_:." and not out.endswith("-"):
            out += "-"
    return out.strip("-")


class Anchors:
    """Each heading's anchor on a page: a repeat gets `-2`, `-3` and so on."""

    def __init__(self):
        self.seen = {}

    def of(self, heading):
        base = slug(heading)
        self.seen[base] = self.seen.get(base, 0) + 1
        n = self.seen[base]
        return base if n == 1 else f"{base}-{n}"


def plain(markdown):
    """A line of Markdown's words, without its marks: what a heading's anchor is made from."""
    t = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", markdown)
    return t.replace("`", "").replace("**", "")


def href(link):
    """A link of a page, for its HTML page: another page's Markdown is its HTML page."""
    if re.match(r"^[a-z]+:", link) or link.startswith("#"):
        return link
    path, _, place = link.partition("#")
    if path.endswith(".md"):
        path = path[:-3] + ".html"
    return path + ("#" + place if place else "")


def inline(markdown):
    """A line of Markdown as HTML: its code, links, bold and italics; everything else escaped."""
    codes = []
    urls = []

    def keep(m):
        codes.append(f"<code>{html.escape(m.group(1), quote=False)}</code>")
        return f"\x00{len(codes) - 1}\x00"

    def link(m):
        # Style a link's label, but keep marks in its destination as URL characters.
        urls.append(html.escape(href(html.unescape(m.group(2)))))
        return f'<a href="\x01{len(urls) - 1}\x01">{m.group(1)}</a>'

    t = re.sub(r"`([^`]+)`", keep, markdown)
    t = html.escape(t, quote=False)
    t = re.sub(r"\[([^\]]+)\]\(([^)\s]+)\)", link, t)
    t = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", t)
    t = re.sub(r"(?<![\w*])\*([^*\s][^*]*?)\*(?![\w*])", r"<em>\1</em>", t)
    t = re.sub("\x00(\\d+)\x00", lambda m: codes[int(m.group(1))], t)
    return re.sub("\x01(\\d+)\x01", lambda m: urls[int(m.group(1))], t)


ITEM = re.compile(r"^( *)([-*+]|\d{1,4}\.) (.*)$")


def cells(row):
    """A table row's cells, keeping code spans and escaped bars in their cell."""
    parts, cell, code = [], [], False
    text = row.strip()
    i = 1 if text.startswith("|") else 0
    while i < len(text):
        c = text[i]
        if c == "\\" and i + 1 < len(text) and text[i + 1] == "|":
            cell.append("|")
            i += 2
            continue
        if c == "`":
            code = not code
        if c == "|" and not code:
            parts.append("".join(cell).strip())
            cell = []
        else:
            cell.append(c)
        i += 1
    if cell:
        parts.append("".join(cell).strip())
    return parts


def render(markdown):
    """A page's Markdown as HTML, its headings with the export's anchors."""
    lines = markdown.split("\n")
    out = []
    anchors = Anchors()
    para = []
    lists = []  # each open list's tag and depth
    last = None  # what the last block was

    def flush():
        nonlocal last
        if para:
            text = " ".join(para)
            meta = last == "heading" and re.match(r"^`[\w ]+` · ", text)
            out.append(f'<p class="meta">{inline(text)}</p>' if meta else f"<p>{inline(text)}</p>")
            para.clear()
            last = "para"

    def close_lists(depth=-1):
        while lists and lists[-1][1] > depth:
            out.append(f"</li></{lists.pop()[0]}>")

    i = 0
    while i < len(lines):
        line = lines[i]
        t = line.strip()
        if t.startswith("```"):
            flush()
            close_lists()
            fence = len(t) - len(t.lstrip("`"))
            code = []
            i += 1
            while i < len(lines):
                closing = lines[i].strip()
                if len(closing) >= fence and not closing.strip("`"):
                    break
                code.append(lines[i])
                i += 1
            out.append(f"<pre><code>{html.escape(chr(10).join(code), quote=False)}</code></pre>")
            last = "code"
            i += 1
            continue
        if not t:
            flush()
            i += 1
            continue
        heading = re.match(r"^(#{1,6}) (.*)$", t)
        if heading:
            flush()
            close_lists()
            level = len(heading.group(1))
            text = heading.group(2).strip()
            anchor = anchors.of(plain(text))
            out.append(f'<h{level} id="{anchor}"><a class="anchor" href="#{anchor}">{inline(text)}</a></h{level}>')
            last = "heading"
            i += 1
            continue
        if t in ("---", "***", "___"):
            flush()
            close_lists()
            out.append("<hr>")
            last = "rule"
            i += 1
            continue
        if t.startswith("|") and i + 1 < len(lines) and re.match(r"^\|[\s|:-]+\|?$", lines[i + 1].strip()) and "-" in lines[i + 1]:
            flush()
            close_lists()
            head = cells(t)
            rows = []
            i += 2
            while i < len(lines) and lines[i].strip().startswith("|"):
                rows.append(cells(lines[i]))
                i += 1
            out.append('<div class="table"><table><thead><tr>' + "".join(f"<th>{inline(c)}</th>" for c in head) + "</tr></thead><tbody>")
            for row in rows:
                out.append("<tr>" + "".join(f"<td>{inline(row[k] if k < len(row) else '')}</td>" for k in range(len(head))) + "</tr>")
            out.append("</tbody></table></div>")
            last = "table"
            continue
        if t.startswith(">"):
            flush()
            close_lists()
            quote = []
            while i < len(lines) and lines[i].strip().startswith(">"):
                quote.append(lines[i].strip()[1:].strip())
                i += 1
            out.append(f"<blockquote><p>{inline(' '.join(quote))}</p></blockquote>")
            last = "quote"
            continue
        item = ITEM.match(line)
        if item:
            flush()
            depth = len(item.group(1)) // 2
            tag = "ol" if item.group(2)[0].isdigit() else "ul"
            text = [item.group(3).strip()]
            i += 1
            while i < len(lines):
                c = lines[i].strip()
                if not c or ITEM.match(lines[i]) or c.startswith(("```", "|", "#")):
                    break
                text.append(c)
                i += 1
            # Deeper: a list inside the item. Shallower: the deeper lists end. The same depth: the
            # next item, or a list of the other kind in its place.
            while lists and lists[-1][1] > depth:
                out.append(f"</li></{lists.pop()[0]}>")
            if lists and lists[-1][1] == depth and lists[-1][0] == tag:
                out.append("</li>")
            else:
                if lists and lists[-1][1] == depth:
                    out.append(f"</li></{lists.pop()[0]}>")
                start = f' start="{item.group(2)[:-1]}"' if tag == "ol" and item.group(2) != "1." else ""
                out.append(f"<{tag}{start}>")
                lists.append((tag, depth))
            out.append(f"<li>{inline(' '.join(text))}")
            last = "item"
            continue
        if lists and not line.startswith(" "):
            close_lists()
        para.append(t)
        i += 1
    flush()
    close_lists()
    return "\n".join(out)


def group_of(page):
    """A page's group, as the docs list it: the language, or its package."""
    return "spec" if page["kind"] == "spec" else page["path"].split("/")[0]


def group_title(group):
    return {"spec": "The language", "std": "Standard library", "engine": "Engine",
            "ui": "Drawing and widgets", "platform": "Host services"}.get(group, group)


def page_html(page, body, before, after):
    """A page of the docs as HTML: the header, its group, its body, and its neighbours."""
    path = page["path"]
    up = "../" * path.count("/")
    name = path.rsplit("/", 1)[-1][:-3]
    live = "/#" + path[:-3]
    title = html.escape(page["title"])
    summary = html.escape(page.get("summary", "").replace("`", ""))
    near = []
    for kind, other, cls in (("Previous", before, "previous"), ("Next", after, "next")):
        if other:
            near.append(f'<a class="{cls}" href="{up}{html.escape(other["path"][:-3])}.html"><span>{kind}</span>{html.escape(other["title"])}</a>')
    return f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>{title} | Wrela docs</title>
<meta name="description" content="{summary}">
<link rel="alternate" type="text/markdown" href="{html.escape(name)}.md">
<link rel="stylesheet" href="{up}pages.css">
</head>
<body>
<header><a class="mark" href="/">WRELA</a><span class="docs">docs</span><a class="live" href="{html.escape(live)}">Live docs</a></header>
<main>
<p class="group">{html.escape(group_title(group_of(page)))}</p>
{body}
</main>
<footer><nav>{"".join(near)}</nav><a class="md" href="{html.escape(name)}.md">This page as Markdown</a></footer>
</body>
</html>
"""


def write_pages(directory):
    """Writes each Markdown page of the export in `directory` as HTML beside it, with the pages'
    style, fonts and script. How many pages."""
    index = json.loads((directory / "index.json").read_text())
    pages = sorted(index.get("pages", []), key=lambda p: (group_of(p) != "spec", group_of(p) != "std", group_of(p)))
    for k, page in enumerate(pages):
        source = directory / page["path"]
        if not source.is_file():
            continue
        body = render(source.read_text())
        before = pages[k - 1] if k > 0 else None
        after = pages[k + 1] if k + 1 < len(pages) else None
        source.with_suffix(".html").write_text(page_html(page, body, before, after))
    (directory / "pages.css").write_text(CSS)
    (directory / "pages.js").write_text(SCRIPT)
    for font, _ in FONTS:
        shutil.copy2(ROOT / font, directory / Path(font).name)
    return len(pages)
