"""Strict registry contracts for the evaluation harness.

Normative source: ``plans/eval-harness/DESIGN.md`` §§2–5. The two JSON
Schema documents under ``schemas/`` are the normative contract; this module
implements the same used subset of JSON Schema 2020-12 with the standard
library so that no third-party dependency is required at build time. When
``jsonschema`` is importable, the test suite runs both validators over the
fixture battery and requires identical accept/reject decisions.

Python 3.14 standard library only. Import only through the canonical
package name ``smoke.eval_harness.contract``.
"""
from __future__ import annotations

import dataclasses
import json
import re
from pathlib import Path
from typing import Any, Mapping

__all__ = [
    "AdapterContext",
    "AdapterResult",
    "CANARY_TOKEN",
    "ContractError",
    "RAW_KEY_PATTERN",
    "canonical_json_bytes",
    "canonical_locator_key",
    "load_catalog",
    "load_schema",
    "sensitive_matches",
    "validate_catalog",
    "validate_evidence_index",
]

_SCHEMA_DIR = Path(__file__).resolve().parent / "schemas"

#: Raw-key expression pinned by the campaign (DESIGN.md §5). Never quote a
#: match: :func:`sensitive_matches` returns pattern names and offsets only.
RAW_KEY_PATTERN = r"sk-[A0-9a-z]{16,}"
CANARY_TOKEN = "canary-key-DO-NOT-LEAK-0123456789"

_RAW_KEY_RE = re.compile(RAW_KEY_PATTERN.encode("ascii"))
_CANARY_RE = re.compile(CANARY_TOKEN.encode("ascii"))


class ContractError(Exception):
    """The sole validation exception of the harness contracts.

    Raised for every catalog, evidence-index, and rooted-path contract
    violation. ``code`` is a stable machine-readable label where one exists.
    """

    def __init__(self, message: str, *, code: str | None = None) -> None:
        super().__init__(message)
        self.code = code


@dataclasses.dataclass(frozen=True)
class AdapterContext:
    """Read-only context handed to every adapter (DESIGN.md §4)."""

    roots: Mapping[str, Path]
    catalog: Mapping[str, object]


@dataclasses.dataclass(frozen=True)
class AdapterResult:
    """Immutable adapter output; validated again at the writer boundary."""

    entities: tuple[dict, ...] = ()
    relationships: tuple[dict, ...] = ()
    runs: tuple[dict, ...] = ()
    findings: tuple[dict, ...] = ()


def canonical_json_bytes(value: Any) -> bytes:
    """Canonical serialization: sorted keys, two-space indent, UTF-8, and
    exactly one trailing newline (DESIGN.md §3 stable record rules).

    Non-finite floats (NaN, Infinity) are not valid JSON values and raise
    ContractError instead of being serialized.
    """
    try:
        text = json.dumps(
            value, sort_keys=True, indent=2, ensure_ascii=False, allow_nan=False
        )
    except ValueError as exc:
        raise ContractError(
            f"non-finite value in canonical JSON: {exc}", code="non-finite-json"
        ) from None
    return (text + "\n").encode("utf-8")


def canonical_locator_key(locator: Mapping[str, Any] | None) -> bytes:
    """Canonical compact JSON of a rooted source locator.

    An absent locator is the canonical empty object ``{}`` (DESIGN.md §3).
    """
    if locator is None:
        return b"{}"
    if not isinstance(locator, Mapping) or not locator:
        raise ContractError(
            "a present locator must be a non-empty mapping", code="locator-shape"
        )
    for key in ("root", "path"):
        if not isinstance(locator.get(key), str) or not locator[key]:
            raise ContractError(
                f"locator key {key!r} must be a non-empty string",
                code="locator-shape",
            )
    try:
        text = json.dumps(
            dict(locator), sort_keys=True, separators=(",", ":"), ensure_ascii=False
        )
    except (TypeError, ValueError) as exc:
        raise ContractError(
            f"locator is not JSON-serializable: {exc}", code="locator-shape"
        ) from None
    return text.encode("utf-8")


def sensitive_matches(data: str | bytes) -> list[dict]:
    """Scan bytes for the pinned raw-key expression and the canary token.

    Returns only ``{"pattern": name, "offset": n}`` records (byte offsets for
    ``bytes``, character offsets for ``str``), sorted by offset then name.
    The matched secret text is never part of the result.
    """
    if isinstance(data, str):
        raw = data.encode("utf-8")
        char_offsets = True
    elif isinstance(data, (bytes, bytearray)):
        raw = bytes(data)
        char_offsets = False
    else:
        raise ContractError(
            "sensitive_matches accepts str or bytes", code="sensitive-input"
        )
    hits: list[tuple[int, str]] = []
    for match in _RAW_KEY_RE.finditer(raw):
        hits.append((match.start(), "raw-key"))
    for match in _CANARY_RE.finditer(raw):
        hits.append((match.start(), "canary"))
    hits.sort()
    if char_offsets:
        # Byte offsets map back to character offsets on the UTF-8 encoding.
        return [
            {"pattern": name, "offset": _byte_to_char(raw, offset)}
            for offset, name in hits
        ]
    return [{"pattern": name, "offset": offset} for offset, name in hits]


def _byte_to_char(data: bytes, byte_offset: int) -> int:
    # A UTF-8 continuation byte is 0b10xxxxxx; character offset = number of
    # lead bytes strictly before the position.
    count = 0
    for i in range(byte_offset):
        if data[i] < 0x80 or data[i] >= 0xC0:
            count += 1
    return count


def load_schema(name: str) -> dict:
    """Load one of the two normative JSON Schema documents."""
    path = _SCHEMA_DIR / name
    with open(path, "rb") as fh:
        return json.loads(fh.read().decode("utf-8"))


def load_catalog() -> dict:
    """Load the hand-maintained ``catalog.json`` (unvalidated)."""
    with open(Path(__file__).resolve().parent / "catalog.json", "rb") as fh:
        return json.loads(fh.read().decode("utf-8"))


def validate_catalog(value: Any) -> None:
    """Validate a registry catalog; return None or raise ContractError."""
    if not isinstance(value, dict):
        raise ContractError("$: catalog must be a JSON object", code="catalog-type")
    _validate(value, load_schema("catalog.schema.json"), "$")
    _semantic_catalog_checks(value)


def validate_evidence_index(value: Any) -> None:
    """Validate an evidence-index document; return None or raise
    ContractError."""
    if not isinstance(value, dict):
        raise ContractError(
            "$: evidence index must be a JSON object", code="index-type"
        )
    _validate(value, load_schema("evidence-index.schema.json"), "$")
    _semantic_index_checks(value)


# --- JSON Schema 2020-12 used-subset evaluator -----------------------------


def _type_ok(instance: Any, type_name: str) -> bool:
    if type_name == "object":
        return isinstance(instance, dict)
    if type_name == "array":
        return isinstance(instance, list)
    if type_name == "string":
        return isinstance(instance, str)
    if type_name == "boolean":
        return isinstance(instance, bool)
    if type_name == "integer":
        return isinstance(instance, int) and not isinstance(instance, bool)
    if type_name == "number":
        return isinstance(instance, (int, float)) and not isinstance(instance, bool)
    if type_name == "null":
        return instance is None
    raise ContractError(
        f"unsupported schema type keyword value {type_name!r}", code="schema-bug"
    )


def _deep_equal(a: Any, b: Any) -> bool:
    if isinstance(a, dict) and isinstance(b, dict):
        return a.keys() == b.keys() and all(_deep_equal(a[k], b[k]) for k in a)
    if isinstance(a, list) and isinstance(b, list):
        return len(a) == len(b) and all(_deep_equal(x, y) for x, y in zip(a, b))
    if isinstance(a, bool) or isinstance(b, bool):
        return isinstance(a, bool) and isinstance(b, bool) and a is b
    return a == b


def _validate(instance: Any, schema: Any, path: str) -> None:
    if schema is True:
        return
    if schema is False:
        raise ContractError(f"{path}: schema is false (no instance valid)")
    if not isinstance(schema, dict):
        raise ContractError(f"{path}: malformed schema", code="schema-bug")

    if "type" in schema:
        types = schema["type"]
        names = types if isinstance(types, list) else [types]
        if not any(_type_ok(instance, name) for name in names):
            raise ContractError(f"{path}: expected type {names!r}")
    if "enum" in schema:
        if not any(_deep_equal(instance, option) for option in schema["enum"]):
            raise ContractError(f"{path}: value not in enum")
    if "const" in schema:
        if not _deep_equal(instance, schema["const"]):
            raise ContractError(f"{path}: value is not const {schema['const']!r}")

    if isinstance(instance, str):
        if "minLength" in schema and len(instance) < schema["minLength"]:
            raise ContractError(f"{path}: shorter than minLength")
        if "maxLength" in schema and len(instance) > schema["maxLength"]:
            raise ContractError(f"{path}: longer than maxLength")
        if "pattern" in schema and re.search(schema["pattern"], instance) is None:
            raise ContractError(f"{path}: does not match pattern")
    if isinstance(instance, (int, float)) and not isinstance(instance, bool):
        if "minimum" in schema and instance < schema["minimum"]:
            raise ContractError(f"{path}: below minimum")
        if "maximum" in schema and instance > schema["maximum"]:
            raise ContractError(f"{path}: above maximum")

    if isinstance(instance, list):
        if "minItems" in schema and len(instance) < schema["minItems"]:
            raise ContractError(f"{path}: fewer than minItems")
        if "maxItems" in schema and len(instance) > schema["maxItems"]:
            raise ContractError(f"{path}: more than maxItems")
        if schema.get("uniqueItems"):
            for i in range(len(instance)):
                for j in range(i + 1, len(instance)):
                    if _deep_equal(instance[i], instance[j]):
                        raise ContractError(f"{path}: items {i} and {j} are duplicates")
        if "items" in schema:
            for index, item in enumerate(instance):
                _validate(item, schema["items"], f"{path}[{index}]")

    if isinstance(instance, dict):
        for key in schema.get("required", ()):
            if key not in instance:
                raise ContractError(f"{path}: required property {key!r} missing")
        properties = schema.get("properties", {})
        for key, sub in properties.items():
            if key in instance:
                _validate(instance[key], sub, f"{path}.{key}")
        additional = schema.get("additionalProperties", True)
        if additional is not True:
            for key in instance:
                if key not in properties:
                    if additional is False:
                        raise ContractError(
                            f"{path}: unknown property {key!r}", code="unknown-key"
                        )
                    _validate(instance[key], additional, f"{path}.{key}")

    if "allOf" in schema:
        for sub in schema["allOf"]:
            _validate(instance, sub, path)
    if "not" in schema and _try_validate(instance, schema["not"]):
        raise ContractError(f"{path}: must not validate against 'not' schema")
    if "anyOf" in schema and not any(
        _try_validate(instance, sub) for sub in schema["anyOf"]
    ):
        raise ContractError(f"{path}: does not match any 'anyOf' branch")
    if "oneOf" in schema:
        matched = sum(1 for sub in schema["oneOf"] if _try_validate(instance, sub))
        if matched != 1:
            raise ContractError(f"{path}: must match exactly one 'oneOf' branch")
    if "if" in schema:
        if _try_validate(instance, schema["if"]):
            if "then" in schema:
                _validate(instance, schema["then"], path)
        elif "else" in schema:
            _validate(instance, schema["else"], path)


def _try_validate(instance: Any, schema: Any) -> bool:
    try:
        _validate(instance, schema, "$")
        return True
    except ContractError:
        return False


# --- semantic cross-checks beyond the expressible schema subset ------------


def _ids(values: list[dict], field: str, label: str) -> None:
    seen: set[str] = set()
    for value in values:
        key = value.get(field)
        if isinstance(key, str) and key in seen:
            raise ContractError(f"duplicate {label} id {key!r}", code="duplicate-id")
        if isinstance(key, str):
            seen.add(key)


def _rooted_root_id(token: str) -> str:
    return token.split(":", 1)[0]


# Selftest argv templates name a supplied named root with the design's
# literal placeholder form "<root-id-root>", e.g. "<plans-root>", either
# alone or at the start of an element followed by "/" and the relative
# path (replaced as one argv element by the supplied named-root path).
_PLACEHOLDER_RE = re.compile(r"^<([a-z][a-z0-9-]*?)-root>(?:/|$)")
_PLACEHOLDER_OCCURRENCE_RE = re.compile(r"<[a-z][a-z0-9-]*?-root>")


def _semantic_catalog_checks(value: dict) -> None:
    roots = value.get("roots", [])
    declared = {r.get("id") for r in roots if isinstance(r, dict)}
    _ids(roots, "id", "root")
    _ids(value.get("inventory_scopes", []), "id", "scope")
    components = value.get("components", [])
    _ids(components, "id", "component")
    for row in components:
        if not isinstance(row, dict):
            continue
        comp_id = row.get("id", "?")
        for index, item in enumerate(row.get("inputs", [])):
            if isinstance(item, str) and ":" in item:
                root_id = _rooted_root_id(item)
                if root_id not in declared:
                    raise ContractError(
                        f"component {comp_id!r} input {index} references undeclared "
                        f"root {root_id!r}",
                        code="cross-root",
                    )
        for index, entry in enumerate(row.get("entrypoints", [])):
            if not isinstance(entry, dict) or entry.get("role") != "selftest":
                continue
            argv = entry.get("argv", [])
            for element in argv:
                if not isinstance(element, str):
                    continue
                for occurrence in _PLACEHOLDER_OCCURRENCE_RE.finditer(element):
                    if occurrence.start() != 0:
                        raise ContractError(
                            f"component {comp_id!r} selftest argv element "
                            f"{element!r} contains a root placeholder that is "
                            f"neither a whole element nor in leading position",
                            code="selftest-placeholder",
                        )
                if not element.startswith("<"):
                    continue
                match = _PLACEHOLDER_RE.match(element)
                if match is None or match.group(1) not in declared:
                    raise ContractError(
                        f"component {comp_id!r} selftest argv placeholder "
                        f"{element!r} does not name a declared root",
                        code="selftest-placeholder",
                    )


#: Each inventory scope is defined against exactly one root (DESIGN.md §2).
_SCOPE_ROOT_PAIRING = {
    "plans-corpus": "plans",
    "plans-declared-inputs": "plans",
    "worktree-declared-inputs": "worktree",
}


def _semantic_index_checks(value: dict) -> None:
    seen: set[str] = set()
    for collection in ("components", "entities", "relationships", "runs", "findings"):
        records = value.get(collection, [])
        for record in records:
            if not isinstance(record, dict):
                continue
            record_id = record.get("id")
            if not isinstance(record_id, str):
                continue
            if record_id in seen:
                raise ContractError(
                    f"duplicate record id {record_id!r} in {collection!r}",
                    code="duplicate-id",
                )
            seen.add(record_id)
    seen_triples: set[tuple[str, str, str]] = set()
    for record in value.get("inventory", []):
        if not isinstance(record, dict):
            continue
        scope = record.get("scope")
        root = record.get("root")
        path = record.get("path")
        if scope in _SCOPE_ROOT_PAIRING and root != _SCOPE_ROOT_PAIRING[scope]:
            raise ContractError(
                f"inventory scope {scope!r} requires root "
                f"{_SCOPE_ROOT_PAIRING[scope]!r}, got {root!r}",
                code="scope-root-mismatch",
            )
        if all(isinstance(part, str) for part in (scope, root, path)):
            triple = (scope, root, path)
            if triple in seen_triples:
                raise ContractError(
                    f"duplicate inventory record for {triple!r}",
                    code="duplicate-inventory",
                )
            seen_triples.add(triple)
    snapshot = value.get("snapshot")
    if isinstance(snapshot, dict):
        for scope in snapshot.get("scopes", []):
            if not isinstance(scope, dict):
                continue
            scope_id = scope.get("id")
            if (
                scope_id in _SCOPE_ROOT_PAIRING
                and scope.get("root") != _SCOPE_ROOT_PAIRING[scope_id]
            ):
                raise ContractError(
                    f"snapshot scope {scope_id!r} requires root "
                    f"{_SCOPE_ROOT_PAIRING[scope_id]!r}, got {scope.get('root')!r}",
                    code="scope-root-mismatch",
                )
