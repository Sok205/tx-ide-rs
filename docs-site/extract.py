"""Parse a crate's `src/*.rs` into items (types, traits, functions, methods, consts) and the
calls between them. Line-based item discovery plus a small lexer that blanks strings and comments,
so braces and names are only counted in code. Heuristic, not a compiler: good enough to draw who
holds and calls what."""

import re

# Traits whose impls are boilerplate, not structure.
SKIPPED_TRAITS = {"Debug", "Display", "From", "TryFrom", "FromStr", "Default", "Error"}

# Method names too generic to link through an unknown receiver (`x.name(`).
GENERIC_METHODS = {
    "new", "get", "len", "is_empty", "push", "insert", "remove", "contains", "iter", "map",
    "clone", "as_str", "join", "path", "dir", "parse", "to_string", "to_owned", "into", "from",
    "collect", "all", "any", "find", "filter", "extend", "write", "read", "lock", "unlock",
    "open", "create", "file", "text", "settings", "config", "socket", "id", "name", "run",
    "fmt", "args", "arg", "env", "output", "status", "session", "matches", "load",
    "save", "delete", "program", "model", "effort", "resume", "profile", "tags", "tag",
    "parent", "lifecycle", "initial_prompt", "workspace", "engine", "sink", "verb", "resource",
    "binary", "conf", "usage", "contains_key", "keys", "values", "entry", "next", "drop",
}

ITEM_START = re.compile(
    r"^(?P<indent>\s*)(?P<visibility>pub(?:\([^)]*\))?\s+)?"
    r"(?P<kind>(?:const\s+)?fn|struct|enum|trait|impl|const|type)\b"
)
CHAR_LITERAL = re.compile(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'\n])'")
RAW_STRING = re.compile(r"b?r(#*)\"")


def mask(text):
    """`text` with the contents of comments, strings and char literals replaced by spaces
    (newlines kept), so offsets and line numbers still line up."""
    output = list(text)
    index, length = 0, len(text)

    def blank(start, end):
        for position in range(start, end):
            if output[position] != "\n":
                output[position] = " "

    while index < length:
        if text.startswith("//", index):
            end = text.find("\n", index)
            end = length if end < 0 else end
            blank(index, end)
            index = end
            continue
        if text.startswith("/*", index):
            end = text.find("*/", index + 2)
            end = length if end < 0 else end + 2
            blank(index, end)
            index = end
            continue
        previous = text[index - 1] if index else " "
        raw = RAW_STRING.match(text, index)
        if raw and not (previous.isalnum() or previous == "_"):
            terminator = '"' + raw.group(1)
            end = text.find(terminator, raw.end())
            end = length if end < 0 else end + len(terminator)
            blank(raw.end(), end - len(terminator))
            index = end
            continue
        character = text[index]
        if character == '"':
            position = index + 1
            while position < length and text[position] != '"':
                position += 2 if text[position] == "\\" else 1
            blank(index + 1, position)
            index = position + 1
            continue
        if character == "'":
            literal = CHAR_LITERAL.match(text, index)
            if literal:
                blank(index + 1, literal.end() - 1)
                index = literal.end()
                continue
        index += 1
    return "".join(output)


def depth_table(masked):
    """Brace depth before every offset."""
    depth = [0] * (len(masked) + 1)
    current = 0
    for position, character in enumerate(masked):
        depth[position] = current
        if character == "{":
            current += 1
        elif character == "}":
            current -= 1
    depth[len(masked)] = current
    return depth


def item_end(masked, start):
    """Offset just past the item that begins at `start`: its first top-level `;`, or the `}`
    matching its first `{`."""
    level = 0
    for position in range(start, len(masked)):
        character = masked[position]
        if character == "{":
            level += 1
        elif character == "}":
            level -= 1
            if level == 0:
                return position + 1
        elif character == ";" and level == 0:
            return position + 1
    return len(masked)


def first_line_above(lines, line_index):
    """The first line of the item including its doc comments and attributes."""
    cursor = line_index
    while cursor > 0 and lines[cursor - 1].strip().startswith(("///", "#[")):
        cursor -= 1
    return cursor


def item_name(kind, header):
    if kind == "impl":
        match = re.match(
            r"impl\s*(?:<.*?>)?\s*(?:(?P<trait>[\w:]+)(?:<[^>]*>)?\s+for\s+)?(?P<type>[\w:]+)",
            header,
        )
        trait = match.group("trait")
        return (trait.split("::")[-1] if trait else None), match.group("type").split("::")[-1]
    return re.search(rf"\b{kind.split()[-1]}\s+(\w+)", header).group(1)


def signature_of(source, masked, start, end):
    """The item's text up to its body: everything before the first `{` (or the whole item)."""
    body = masked.find("{", start, end)
    head = source[start:body if body >= 0 else end]
    signature = re.sub(r"\s+", " ", head).strip().rstrip(";").strip().rstrip(",")
    return re.sub(r",\s*([)>])", r"\1", re.sub(r"([(<])\s+", r"\1", signature))


def parse_module(path):
    module = path.stem
    full = path.read_text()
    tests = full.find("\n#[cfg(test)]")
    source = full if tests < 0 else full[: tests + 1]
    masked = mask(source)
    depth = depth_table(masked)
    lines = source.split("\n")
    line_offsets = [0]
    for line in lines:
        line_offsets.append(line_offsets[-1] + len(line) + 1)
    items, impls = [], []

    for line_index, line in enumerate(lines):
        offset = line_offsets[line_index]
        match = ITEM_START.match(line)
        if not match or masked[offset: offset + len(line)].strip() == "":
            continue
        keyword_offset = offset + match.start("kind")
        level = depth[keyword_offset]
        if level > 1:
            continue
        kind = re.sub(r"\s+", " ", match.group("kind"))
        end = item_end(masked, keyword_offset)
        body = masked.find("{", keyword_offset, end)
        header = re.sub(r"\s+", " ", source[keyword_offset: body if body >= 0 else end])
        visibility = (match.group("visibility") or "").strip()
        record = {
            "module": module,
            "kind": "fn" if kind.endswith("fn") else kind,
            "public": bool(visibility) and "crate" not in visibility,
            "visibility": visibility,
            "level": level,
            "start": keyword_offset,
            "end": end,
        }
        if kind == "impl":
            if level == 0:
                record["trait"], record["owner"] = item_name(kind, header)
                impls.append(record)
            continue
        record["name"] = item_name(kind, header)
        record["signature"] = signature_of(source, masked, keyword_offset, end)
        last_line = source.count("\n", 0, end - 1)
        record["code"] = "\n".join(lines[first_line_above(lines, line_index): last_line + 1])
        record["masked_body"] = masked[keyword_offset:end]
        items.append(record)

    # Functions one level down belong to the impl or trait block that encloses them.
    containers = impls + [item for item in items if item["kind"] == "trait" and item["level"] == 0]
    attached = []
    for item in items:
        if item["level"] == 0:
            attached.append(item)
            continue
        owner = next((block for block in containers if block["start"] < item["start"] < block["end"]), None)
        if owner is None or item["kind"] != "fn":
            continue
        if owner["kind"] == "trait":
            item.update(owner=owner["name"], trait=owner["name"], in_trait_definition=True, public=owner["public"])
        else:
            if owner["trait"] in SKIPPED_TRAITS:
                continue
            item.update(owner=owner["owner"], trait=owner["trait"], in_trait_definition=False)
            if owner["trait"]:
                item["public"] = True
        attached.append(item)
    imports = sorted(set(re.findall(r"\bcrate::([a-z_]+)", masked)) - {module})
    return {"module": module, "imports": imports, "items": attached}


def build(source_dir, module_names):
    """Items of the given modules (`<name>.rs` under `source_dir`) and the calls between them."""
    modules = [parse_module(source_dir / f"{name}.rs") for name in module_names]
    nodes = []
    for module in modules:
        for item in module["items"]:
            owner = item.get("owner")
            title = f"{owner}::{item['name']}" if owner else item["name"]
            kind = "method" if owner and item["kind"] == "fn" else item["kind"]
            nodes.append({**item, "title": title, "kind": kind})
    seen_titles = set()
    for node in nodes:
        if node["title"] in seen_titles:
            node["title"] = f"{node['title']} ({node['module']})"
        seen_titles.add(node["title"])
        node["id"] = f"{node['module']}/{node['title']}"

    type_names = {node["name"] for node in nodes if node["kind"] in ("struct", "enum", "trait", "type")}
    methods_by_owner, methods_by_name, free_functions = {}, {}, {}
    for node in nodes:
        if node["kind"] == "method":
            methods_by_owner.setdefault(node["owner"], {}).setdefault(node["name"], []).append(node)
            methods_by_name.setdefault(node["name"], []).append(node)
        if node["kind"] == "fn":
            free_functions.setdefault(node["name"], []).append(node)

    # Struct field → crate type, to resolve `self.field.method(`.
    field_types = {}
    for node in nodes:
        if node["kind"] != "struct":
            continue
        fields = {}
        for field, type_text in re.findall(r"(?:pub(?:\([^)]*\))?\s+)?(\w+)\s*:\s*([^,\n]+)", node["masked_body"].split("{", 1)[-1]):
            known = [candidate for candidate in re.findall(r"[A-Z]\w*", type_text) if candidate in type_names or candidate in methods_by_owner]
            if known:
                fields[field] = known[0]
        field_types[node["name"]] = fields

    def method_on(owner, name):
        candidates = methods_by_owner.get(owner, {}).get(name, [])
        return candidates[0] if candidates else None

    def by_unique_method_name(name):
        if name in GENERIC_METHODS:
            return None
        candidates = methods_by_name.get(name, [])
        declared = [node for node in candidates if node.get("in_trait_definition")]
        if len(declared) == 1:
            return declared[0]
        return candidates[0] if len(candidates) == 1 else None

    calls = set()

    def call(source, target):
        if target is not None and target is not source:
            calls.add((source["id"], target["id"]))

    for node in nodes:
        body = node["masked_body"]
        owner = node.get("owner")
        for type_name, member in re.findall(r"\b([A-Z]\w*)::(\w+)", body):
            call(node, method_on(owner if type_name == "Self" else type_name, member))
        for name in re.findall(r"\bself\s*\.\s*(\w+)\s*\(", body):
            call(node, method_on(owner, name))
        own_fields = field_types.get(owner, {})
        for field, name in re.findall(r"\bself\s*\.\s*(\w+)\s*\.\s*(\w+)\s*\(", body):
            if field in own_fields:
                call(node, method_on(own_fields[field], name))
        for path_module, name in re.findall(r"\b(?:crate::)?([a-z_]+)::([a-z_]\w*)\s*\(", body):
            for candidate in free_functions.get(name, []):
                if candidate["module"] == path_module:
                    call(node, candidate)
        for name in re.findall(r"(?<![.\w:])(?<!fn )([a-z_]\w*)\s*\(", body):
            candidates = free_functions.get(name, [])
            same_module = [candidate for candidate in candidates if candidate["module"] == node["module"]]
            call(node, same_module[0] if same_module else (candidates[0] if len(candidates) == 1 else None))
        for receiver, name in re.findall(r"(?<!self)\b(\w+)\s*\.\s*(\w+)\s*\(", body):
            # `self.field.method(` is resolved through the field's type above.
            if receiver != "self" and receiver not in own_fields:
                call(node, by_unique_method_name(name))

    imports = {module["module"]: module["imports"] for module in modules}
    return nodes, sorted(calls), imports
