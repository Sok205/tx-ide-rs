# tx-core docs with a structure graph

`cargo doc` for `tx-core`, twice, with a **Structure** drawer added to every rustdoc page:

- **internal**: `--document-private-items --document-hidden-items`, so every item appears, including the `#[doc(hidden)]` modules (`tmux`, `store`, `locks`, `messaging`). The graph shows field, signature and call relations.
- **public**: plain `cargo doc`, which is what a crate user sees. The graph shows relations through public fields and public signatures only.

The drawer is a square in the page's top-right corner. It follows rustdoc's fonts and theme, and toggles with `g` or its tab.

- **Modules**: the module map. Click a header to expand a module into its items.
- **Focus**: the object on the page in the middle, what uses it on the left, and what it uses on the right. Opening an object switches to Focus; a module page or the crate root switches to Modules.
- **Interactions:**
  - Click an item to open its rustdoc page.
  - Hover an item, a module, or a link in the rustdoc text to isolate its connections.
  - ⚙ holds the Internal / Public switch, the edge toggles, the layout buttons and the legend.

## Build

```sh
python3 docs-site/build.py . /tmp/tx-core-docs "tx-core @ $(git rev-parse --short HEAD)"
open /tmp/tx-core-docs/internal/tx_core/index.html
```

This needs the stable Rust toolchain and Python 3, with no Python packages. The internal build uses the unstable `--document-hidden-items` through `RUSTC_BOOTSTRAP=1`. Cargo output goes to `target/structure/`.

The site directory gets:

- `internal/` and `public/`: the two rustdoc trees, each with its `structure.html`.
- `index.html`: forwards to the internal docs.
- `.nojekyll`.

## Hosting

`.github/workflows/docs-site.yml` builds the site on every push to `main` (and on a manual run
from the Actions tab) and deploys it to GitHub Pages: <https://sok205.github.io/tx-ide-rs/>.

`graph.py`'s `LAYOUT_ROWS` places every tx-core module in the Modules view. `build.py` stops with
an error when it no longer matches the `pub mod` lines of `tx-core/src/lib.rs`, so adding,
renaming or removing a module means adding, renaming or removing it there too (and in
`MODULE_COLORS`).

## How it works

- **`extract.py`** parses `tx-core/src/*.rs` with a small lexer: the items, which impl or trait each function belongs to, and calls between functions.
  - It is a heuristic, not the compiler. Calls through generic receivers can be missed, or be linked to the wrong item.
- **`graph.py`** builds each view's cards and edges.
  - Edge kinds: *has* (a field or variant holds the type), *uses* (a declaration or signature mentions it) and *calls*.
  - It also holds the module layout and colours, which are specific to tx-core.
- **`build.py`** does the assembly:
  - Runs rustdoc with `--html-after-content drawer.html`.
  - Maps every card to the page rustdoc wrote for it (including `#method.x` anchors).
  - Writes `structure.html` into each tree, with the data and rustdoc's font files inlined.
- **`drawer.html`** runs in each rustdoc page.
  - It loads `structure.html` in an iframe and sends it the page being read, the page's theme variables and hovered links.
  - In the other direction, the graph asks the page to navigate.
