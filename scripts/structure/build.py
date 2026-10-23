"""Build tx-core's docs site: rustdoc twice (public, and internal with private and #[doc(hidden)]
items), each page extended with the Structure drawer, plus the graph page each drawer loads.

Usage: python3 scripts/structure/build.py <poc workspace> <site dir> <label>
Writes <site>/public, <site>/internal, <site>/index.html and <site>/.nojekyll (GitHub Pages)."""

import hashlib
import html
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import extract
import graph

HERE = Path(__file__).parent
VIEW_FLAGS = {
    "public": "",
    # --document-hidden-items is unstable; RUSTC_BOOTSTRAP lets the stable toolchain use it.
    "internal": "-Z unstable-options --document-private-items --document-hidden-items",
}
PAGE_PREFIX = {"struct": "struct", "enum": "enum", "trait": "trait", "fn": "fn", "const": "constant", "type": "type"}
FONTS = [("Fira Sans", 400, "FiraSans-Regular"), ("Fira Sans", 500, "FiraSans-Medium"),
         ("Source Code Pro", 400, "SourceCodePro-Regular"), ("Source Serif 4", 400, "SourceSerif4-Regular")]


def build_docs(workspace, site, view):
    drawer = (HERE / "drawer.html").read_text()
    target = workspace / "target" / "structure" / view
    target.mkdir(parents=True, exist_ok=True)
    # The drawer's hash is in the flags, so cargo documents again whenever the drawer changes.
    after_content = target / f"drawer-{hashlib.sha1(drawer.encode()).hexdigest()[:10]}.html"
    after_content.write_text(drawer)
    environment = dict(os.environ, RUSTDOCFLAGS=f"{VIEW_FLAGS[view]} --html-after-content {after_content}".strip())
    if view == "internal":
        environment["RUSTC_BOOTSTRAP"] = "1"
    subprocess.run(["cargo", "doc", "-p", "tx-core", "--no-deps", "--target-dir", str(target)],
                   cwd=workspace, env=environment, check=True)
    destination = site / view
    shutil.rmtree(destination, ignore_errors=True)
    shutil.copytree(target / "doc", destination)
    return destination


def page_urls(view, doc_root):
    """Card id → the rustdoc page written for it (relative to the doc root)."""
    pages = {}
    for card in view["cards"]:
        if card["kind"] == "method":
            continue
        name = f"{PAGE_PREFIX[card['kind']]}.{card['name']}.html"
        # Items of a public module live under it; rustdoc inlines re-exports from hidden modules at the root.
        for candidate in (f"tx_core/{card['module']}/{name}", f"tx_core/{name}"):
            if (doc_root / candidate).exists():
                pages[card["id"]] = candidate
                break
    page_text = {}
    for card in view["cards"]:
        if card["kind"] != "method" or card["owner"] not in pages:
            continue
        page = pages[card["owner"]]
        text = page_text.setdefault(page, (doc_root / page).read_text())
        for prefix in ("method", "tymethod"):
            if f'id="{prefix}.{card["name"]}"' in text:
                pages[card["id"]] = f"{page}#{prefix}.{card['name']}"
                break
    for module in graph.MODULE_NAMES:
        if (doc_root / f"tx_core/{module}/index.html").exists():
            pages["module/" + module] = f"tx_core/{module}/index.html"
    return pages


def font_faces(doc_root):
    """rustdoc's own fonts (their file names carry a hash) for the graph page."""
    faces = []
    for family, weight, stem in FONTS:
        file = next((doc_root / "static.files").glob(f"{stem}-*.woff2"))
        faces.append(f'@font-face {{ font-family: "{family}"; font-weight: {weight}; src: url("static.files/{file.name}") format("woff2"); }}')
    return "\n".join(faces)


def main():
    workspace, site, label = Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve(), sys.argv[3]
    site.mkdir(parents=True, exist_ok=True)
    source_dir = workspace / "tx-core" / "src"
    nodes, calls, imports = extract.build(source_dir, graph.MODULE_NAMES)
    crate = graph.crate_root((source_dir / "lib.rs").read_text())
    doc_roots = {view: build_docs(workspace, site, view) for view in ("internal", "public")}
    all_pages = {view: sorted(str(path.relative_to(root)) for path in root.rglob("*.html")) for view, root in doc_roots.items()}
    template = (HERE / "structure.html").read_text()
    for view_name, doc_root in doc_roots.items():
        view = graph.build_view(view_name, nodes, calls, imports, crate)
        view["urls"] = page_urls(view, doc_root)
        view["otherPages"] = all_pages["public" if view_name == "internal" else "internal"]
        payload = json.dumps(view, ensure_ascii=False).replace("</", "<\\/")
        page = template.replace("/*__DATA__*/", payload).replace("/*__FONTS__*/", font_faces(doc_root))
        (doc_root / "structure.html").write_text(page)
        items = sum(card["kind"] != "method" for card in view["cards"])
        print(f"{view_name}: {items} items, {len(view['cards']) - items} methods, {len(view['edges'])} edges, "
              f"{len(view['urls'])} linked to rustdoc pages")
    (site / "index.html").write_text(
        '<!doctype html>\n<meta charset="utf-8">\n<title>tx-core docs</title>\n'
        '<meta http-equiv="refresh" content="0; url=internal/tx_core/index.html">\n'
        f'<p>tx-core docs ({html.escape(label)}): <a href="internal/tx_core/index.html">internal</a> · '
        '<a href="public/tx_core/index.html">public</a></p>\n')
    (site / ".nojekyll").touch()
    print(f"open {site / 'internal' / 'tx_core' / 'index.html'}")


if __name__ == "__main__":
    main()
