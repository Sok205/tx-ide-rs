"""The structure graph of tx-core for one doc view.

`internal`: every item, with field, signature and call relations. `public`: what a user of the
crate can see, with relations through public fields and public signatures only."""

import re

# Module layout of the map, top to bottom; also the order modules are parsed and listed in.
LAYOUT_ROWS = [["plugin"], ["core"], ["config", "profile", "engine", "events"], ["messaging", "tmux", "store", "locks"], ["session", "error"]]
MODULE_COLORS = {
    "core": "#8b7cf6", "config": "#5fa8e8", "session": "#3fc1b0", "store": "#63c174", "locks": "#e5a54b",
    "tmux": "#ef6f6c", "events": "#e879b9", "messaging": "#f0c75e", "profile": "#9fb0c8",
    "engine": "#4fd1e8", "plugin": "#c29ffa", "error": "#d98b62",
}
MODULE_NAMES = [module for row in LAYOUT_ROWS for module in row]


def crate_root(lib_text):
    """From lib.rs: names re-exported at the crate root (stable, and `#[doc(hidden)]`) and the
    `#[doc(hidden)]` modules."""
    stable, hidden, hidden_modules = set(), set(), set()
    lines = lib_text.split("\n")

    def hidden_above(index):
        above = index - 1
        while above >= 0 and lines[above].startswith("///"):
            above -= 1
        return above >= 0 and lines[above].startswith("#[doc(hidden)]")

    index = 0
    while index < len(lines):
        line = lines[index]
        module = re.match(r"pub mod (\w+);", line)
        if module and hidden_above(index):
            hidden_modules.add(module.group(1))
        if line.startswith("pub use "):
            first, statement = index, line
            while not statement.rstrip().endswith(";"):
                index += 1
                statement += " " + lines[index]
            _, _, names = statement[len("pub use "):].rstrip().rstrip(";").partition("::")
            (hidden if hidden_above(first) else stable).update(re.findall(r"\w+", names))
        index += 1
    return stable, hidden, hidden_modules


def strip_docs(code):
    lines = code.split("\n")
    while lines and lines[0].strip().startswith("///"):
        lines.pop(0)
    return "\n".join(lines)


def strip_error_attributes(code):
    """`#[error("…")]` attributes (possibly multi-line) are message text, not structure."""
    output, depth, skipping = [], 0, False
    for line in code.split("\n"):
        if not skipping and line.strip().startswith("#[error("):
            skipping, depth = True, 0
        if skipping:
            depth += line.count("(") + line.count("[") - line.count(")") - line.count("]")
            skipping = depth > 0
            continue
        output.append(line)
    return "\n".join(output)


def public_struct_declaration(code):
    """The struct as a crate user sees it: public fields only."""
    code = strip_docs(code)
    tuple_struct = re.match(r"(?s)(.*?pub struct \w+)\((.*)\);\s*$", code)
    if tuple_struct:
        inner = tuple_struct.group(2)
        return f"{tuple_struct.group(1)}({inner if inner.strip().startswith('pub') else ''});"
    if "{" not in code:
        return code
    head, body = code.split("{", 1)
    public_fields = [line for line in body.rsplit("}", 1)[0].split("\n")
                     if line.strip().startswith("pub ") and not line.strip().startswith("pub(crate)")]
    return head + "{\n" + "\n".join(public_fields) + "\n}"


def with_visibility(node):
    return (node["visibility"] + " " if node["visibility"] else "") + node["signature"]


def declaration_of(node, methods, internal):
    kind = node["kind"]
    if kind == "trait":
        return "\n".join([with_visibility(node) + " {"] + [f"    {method['signature']};" for method in methods] + ["}"])
    if kind == "struct":
        return strip_docs(node["code"]) if internal else public_struct_declaration(node["code"])
    if kind == "enum":
        return strip_error_attributes(strip_docs(node["code"]))
    if kind in ("const", "type"):
        return strip_docs(node["code"])
    return with_visibility(node) + ";"


def declaration_uses(declaration, own_id, type_ids):
    """(target id, via label) for every crate type a declaration line mentions."""
    found = []
    for line in declaration.split("\n"):
        stripped = line.strip()
        if not stripped or stripped.startswith(("///", "#[", "/*")):
            continue
        names = {name for name in re.findall(r"\b([A-Z]\w+)\b", stripped) if name in type_ids}
        if not names:
            continue
        function = re.search(r"\bfn\s+(\w+)", stripped)
        field = re.match(r"(?:pub(?:\([^)]*\))?\s+)?([a-z_]\w*)\s*:", stripped)
        variant = re.match(r"([A-Z]\w*)\s*[({,]", stripped)
        if function:
            via = f"fn {function.group(1)}"
        elif re.match(r"(pub\s+)?(struct|enum|trait|type)\b", stripped):
            via = "bounds"
        elif field:
            via = f"field {field.group(1)}"
        elif variant:
            via = f"variant {variant.group(1)}"
        else:
            via = "declaration"
        found.extend((type_ids[name], via) for name in names if type_ids[name] != own_id)
    return found


def visibility_of(node, stable, hidden, hidden_modules):
    """public: re-exported at the root. module-path: pub, reachable only as `module::item`.
    hidden: `#[doc(hidden)]`. crate / private: not pub."""
    visibility = node["visibility"]
    if node["kind"] != "method" and node["name"] in stable:
        return "public"
    if not visibility:
        return "private"
    if "crate" in visibility or "super" in visibility:
        return "crate"
    if node["module"] in hidden_modules or node["name"] in hidden:
        return "hidden"
    return "module-path"


def build_view(view, nodes, calls, imports, crate):
    internal = view == "internal"
    stable, hidden, hidden_modules = crate
    items = []
    for node in nodes:
        if node["kind"] == "method":
            continue
        visibility = visibility_of(node, stable, hidden, hidden_modules)
        if internal or visibility in ("public", "module-path"):
            items.append((node, visibility))
    item_names = {node["name"] for node, _ in items}

    def methods_of(owner, owner_visibility):
        result = []
        for node in nodes:
            if node["kind"] != "method" or node["owner"] != owner["name"]:
                continue
            if node.get("trait") and node["trait"] != owner["name"] and node["trait"] not in item_names:
                continue
            in_trait = owner["kind"] == "trait"
            if not internal and not (node["public"] or in_trait):
                continue
            if in_trait or node.get("trait") in item_names or node["public"]:
                visibility = owner_visibility
            else:
                visibility = "crate" if "crate" in node["visibility"] else "private"
            result.append((node, visibility))
        return result

    cards, method_cards, declarations = [], [], {}
    for node, visibility in items:
        methods = methods_of(node, visibility) if node["kind"] in ("struct", "enum", "trait") else []
        declarations[node["id"]] = declaration_of(node, [method for method, _ in methods], internal)
        cards.append({"id": node["id"], "name": node["name"], "title": node["title"], "kind": node["kind"],
                      "module": node["module"], "visibility": visibility})
        for method, method_visibility in methods:
            declarations[method["id"]] = (method["signature"] if node["kind"] == "trait" else with_visibility(method)) + ";"
            method_cards.append({"id": method["id"], "name": method["name"], "title": method["title"], "kind": "method",
                                 "module": method["module"], "visibility": method_visibility, "owner": node["id"]})

    type_ids = {card["name"]: card["id"] for card in cards if card["kind"] in ("struct", "enum", "trait", "type")}
    for card in method_cards:
        mentioned = set(re.findall(r"\b([A-Z]\w+)\b", declarations[card["id"]]))
        card["mentions"] = sorted(type_ids[name] for name in mentioned if name in type_ids and type_ids[name] != card["id"])

    # has: a field or variant holds the target. uses: a declaration line or a method signature
    # mentions it. calls (internal only): some function of the source calls into the target.
    relations = {}

    def relate(source, target, kind, label):
        if source != target:
            labels = relations.setdefault((source, target, kind), [])
            if label not in labels:
                labels.append(label)

    methods_by_owner = {}
    for card in method_cards:
        methods_by_owner.setdefault(card["owner"], []).append(card)
    for card in cards:
        for target, via in declaration_uses(declarations[card["id"]], card["id"], type_ids):
            relate(card["id"], target, "has" if via.startswith(("field ", "variant ")) else "uses", via)
        for method in methods_by_owner.get(card["id"], []):
            for target, _ in declaration_uses(declarations[method["id"]], card["id"], type_ids):
                relate(card["id"], target, "uses", f"fn {method['name']}")
    all_cards = {card["id"]: card for card in cards + method_cards}
    call_edges = []
    if internal:
        for source, target in calls:
            if source not in all_cards or target not in all_cards:
                continue
            call_edges.append({"source": source, "target": target})
            source_card, target_card = all_cards[source], all_cards[target]
            relate(source_card.get("owner", source), target_card.get("owner", target), "calls",
                   f"{source_card['name']} → {target_card['title']}")
    edges = [{"source": source, "target": target, "kind": kind, "via": labels}
             for (source, target, kind), labels in sorted(relations.items())]
    module_imports = [{"source": module, "target": target}
                      for module, targets in imports.items() for target in targets if target in MODULE_NAMES]
    return {"view": view, "cards": cards + method_cards, "edges": edges, "callEdges": call_edges,
            "imports": module_imports, "rows": LAYOUT_ROWS, "colors": MODULE_COLORS}
