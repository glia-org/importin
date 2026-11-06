#!/usr/bin/env python3
"""Checks the website in site/ before a commit: python3 checks/site.py [--allow-placeholders]

- nothing is loaded from another origin: no external src=, no external stylesheet or other <link> load, no
  @import, no url(http...) in CSS, inline styles or SVG files;
- every http(s) URL in the HTML is a plain <a href> link; the one exception is <link rel="canonical"> under
  https://glia-org.github.io/importin/;
- no measurement placeholder of the form ⟨measure:…⟩ is left, unless --allow-placeholders is given;
- the page's text has none of the banned words (blazing, powerful, seamless) and no "!" at a sentence end;
- every local file and every #fragment the page refers to exists; no id is used twice;
- every element that is opened is closed, in order; scripts, if any, make no network requests;
- lists the external links (they must stay links) and the size of every file the page loads.

Standard library only. Exits with 1 when a check fails. Adapted from glia's tools/check.py.
"""
import os
import re
import sys
from html.parser import HTMLParser
from urllib.parse import urlsplit

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SITE_DIR = os.path.join(ROOT, "site")
SITE = "https://glia-org.github.io/importin/"
VOID = {"area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr"}
# inside inline SVG, elements may close themselves
SVG_SELF = {"path", "rect", "circle", "line", "polyline", "polygon", "ellipse", "use", "stop"}
LOADS = {("script", "src"), ("img", "src"), ("img", "srcset"), ("iframe", "src"), ("video", "src"),
         ("video", "poster"), ("audio", "src"), ("source", "src"), ("source", "srcset"), ("track", "src"),
         ("embed", "src"), ("object", "data"), ("input", "src"), ("image", "href"), ("use", "href")}
LINK_LOADS = {"stylesheet", "preload", "modulepreload", "icon", "apple-touch-icon", "manifest", "prefetch",
              "dns-prefetch", "preconnect"}
URL_ATTRS = ("src", "href", "srcset", "poster", "data", "action", "formaction", "xlink:href")
BANNED = re.compile(r"\b(blazing\w*|powerful\w*|seamless\w*)", re.I)
BANG = re.compile(r"[\w)\]\"'”’]!+(?=\s|$|[\"'”’)\]])")
PLACEHOLDER = re.compile(r"⟨measure:[^⟩]*⟩")
URL_IN_VALUE = re.compile(r"^\s*(https?:)?//", re.I)

errors, links, loaded = [], [], set()


def external(url):
    return bool(urlsplit(url).scheme) or url.startswith("//")


def local_file(url, base):
    path = urlsplit(url).path
    return os.path.normpath(os.path.join(base, path))


def rel(path):
    return os.path.relpath(path, ROOT)


def check_css_text(text, where, base):
    for _ in re.finditer(r"@import\b", text):
        errors.append(f"{where}: @import (inline it instead)")
    for m in re.finditer(r"url\(\s*['\"]?([^'\")]+)", text):
        url = m.group(1).strip()
        if url.startswith(("data:", "#")):
            continue
        if external(url):
            errors.append(f"{where}: url({url}) loads from another origin")
            continue
        f = local_file(url, base)
        if not os.path.exists(f):
            errors.append(f"{where}: url({url}) does not exist")
        else:
            loaded.add(f)


def check_js_text(text, where):
    for word in ("fetch(", "XMLHttpRequest", "WebSocket", "EventSource", "sendBeacon", "import(", "importScripts"):
        if word in text:
            errors.append(f"{where}: uses {word}")
    for m in re.finditer(r"https?://[^\s'\")]+", text):
        errors.append(f"{where}: contains the URL {m.group(0)}")


class Page(HTMLParser):
    def __init__(self, path):
        super().__init__(convert_charrefs=True)
        self.path, self.base = path, os.path.dirname(path)
        self.stack, self.ids, self.frags = [], {}, []
        self.text, self.raw = [], None  # visible text; the tag whose raw content is being read (style, script)
        self.raw_buf = []

    def where(self):
        return f"{rel(self.path)}:{self.getpos()[0]}"

    def handle_starttag(self, tag, attrs):
        self.attrs(tag, attrs)
        if tag in ("style", "script"):
            self.raw, self.raw_buf = tag, []
        if tag not in VOID:
            self.stack.append((tag, self.getpos()[0]))

    def handle_startendtag(self, tag, attrs):
        self.attrs(tag, attrs)
        if tag not in VOID and tag not in SVG_SELF:
            errors.append(f"{self.where()}: <{tag}/> is not a void element")

    def attrs(self, tag, attrs):
        a = {k: (v or "") for k, v in attrs}
        if "id" in a:
            if a["id"] in self.ids:
                errors.append(f"{self.where()}: id {a['id']!r} also on line {self.ids[a['id']]}")
            self.ids.setdefault(a["id"], self.getpos()[0])
        relset = set(a.get("rel", "").split())
        for name in ("alt", "aria-label", "title", "content", "placeholder"):
            if a.get(name) and not self.raw:
                self.text.append((self.getpos()[0], a[name]))
        for name, value in a.items():
            if name == "xmlns" or name.startswith("xmlns:"):
                continue  # a namespace name, never fetched
            if name == "style":
                check_css_text(value, self.where(), self.base)
            if name in URL_ATTRS:
                for url in (value.split(",") if name == "srcset" else [value]):
                    url = url.strip().split(" ")[0]
                    if url:
                        self.ref(tag, name, url, relset)
            elif URL_IN_VALUE.search(value):
                errors.append(f"{self.where()}: <{tag} {name}=\"{value}\"> holds a URL; only <a href> may link out")

    def ref(self, tag, name, url, relset):
        if url.startswith("#"):
            self.frags.append((url[1:], self.getpos()[0]))
            return
        if url.startswith("javascript:"):
            errors.append(f"{self.where()}: javascript: URL")
            return
        if url.startswith(("mailto:", "data:")):
            return
        loads = (tag, name) in LOADS or (tag == "link" and relset & LINK_LOADS)
        if external(url):
            if tag == "link" and relset == {"canonical"}:
                if not url.startswith(SITE):
                    errors.append(f"{self.where()}: canonical {url} is not under {SITE}")
                return
            if loads:
                errors.append(f"{self.where()}: <{tag} {name}> loads {url} from another origin")
            elif tag == "a" and name == "href":
                links.append(url)
            else:
                errors.append(f"{self.where()}: <{tag} {name}=\"{url}\"> links out; only <a href> may")
            return
        f = local_file(url, self.base)
        if not os.path.exists(f):
            errors.append(f"{self.where()}: {url} does not exist")
        elif loads:
            loaded.add(f)

    def handle_endtag(self, tag):
        if tag == self.raw:
            body = "".join(self.raw_buf)
            if tag == "style":
                check_css_text(body, self.where(), self.base)
            else:
                check_js_text(body, self.where())
            self.raw = None
        if tag in VOID:
            return
        if not self.stack or self.stack[-1][0] != tag:
            open_ = self.stack[-1] if self.stack else ("nothing", 0)
            errors.append(f"{self.where()}: </{tag}> closes <{open_[0]}> from line {open_[1]}")
            for i in range(len(self.stack) - 1, -1, -1):
                if self.stack[i][0] == tag:
                    del self.stack[i:]
                    break
            return
        self.stack.pop()

    def handle_data(self, data):
        if self.raw:
            self.raw_buf.append(data)
        else:
            self.text.append((self.getpos()[0], data))

    def close(self):
        super().close()
        for tag, line in self.stack:
            errors.append(f"{rel(self.path)}:{line}: <{tag}> is never closed")
        for frag, line in self.frags:
            if frag not in self.ids:
                errors.append(f"{rel(self.path)}:{line}: #{frag} has no element")


def check_words(page):
    for line, data in page.text:
        for m in BANNED.finditer(data):
            errors.append(f"{rel(page.path)}:{line}: banned word {m.group(0)!r}")
        for m in BANG.finditer(data):
            errors.append(f"{rel(page.path)}:{line}: '!' at a sentence end: {data.strip()[:60]!r}")


def check_placeholders(path, allow):
    found = []
    for n, line in enumerate(open(path, encoding="utf-8"), 1):
        found += [(n, m.group(0)) for m in PLACEHOLDER.finditer(line)]
    if found and not allow:
        for n, ph in found:
            errors.append(f"{rel(path)}:{n}: placeholder {ph} not filled")
    return found


def check_svg(path):
    text = open(path, encoding="utf-8").read()
    check_css_text(text, rel(path), os.path.dirname(path))
    for m in re.finditer(r"(?<![\w:])(?:xlink:)?href\s*=\s*['\"]\s*((?:https?:)?//[^'\"]*)", text):
        errors.append(f"{rel(path)}: href {m.group(1)} loads from another origin")


def main(argv):
    allow = "--allow-placeholders" in argv
    unknown = [a for a in argv if a != "--allow-placeholders"]
    if unknown:
        print(__doc__)
        return 2
    pages = sorted(os.path.join(d, f) for d, _, fs in os.walk(SITE_DIR) for f in fs if f.endswith(".html"))
    placeholders = []
    for path in pages:
        page = Page(path)
        page.feed(open(path, encoding="utf-8").read())
        page.close()
        check_words(page)
        placeholders += check_placeholders(path, allow)
    for f in sorted(loaded):
        if f.endswith(".css"):
            check_css_text(open(f, encoding="utf-8").read(), rel(f), os.path.dirname(f))
    for f in sorted(loaded):
        if f.endswith(".js"):
            check_js_text(open(f, encoding="utf-8").read(), rel(f))
    for d, _, fs in os.walk(SITE_DIR):
        for f in fs:
            if f.endswith(".svg"):
                check_svg(os.path.join(d, f))

    print("external links (must stay links):")
    for url in sorted(set(links)):
        print("  " + url)
    print("files the pages load:")
    total = 0
    for path in pages:
        total += os.path.getsize(path)
        print(f"  {os.path.getsize(path):>8}  {rel(path)}")
    for f in sorted(loaded):
        n = os.path.getsize(f)
        total += n
        print(f"  {n:>8}  {rel(f)}")
    print(f"  {total:>8}  total")
    if placeholders and allow:
        print(f"\n{len(placeholders)} placeholder(s) left, allowed by --allow-placeholders")
    if errors:
        print(f"\n{len(errors)} problem(s):")
        for e in errors:
            print("  " + e)
        return 1
    print("\nok")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
