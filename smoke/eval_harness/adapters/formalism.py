"""The ``formalism`` adapter (DESIGN.md §§3–4).

Parses the parity-formalism corpus into namespaced, source-linked
entities:

- the 10 Python-linter rules (H-1..H-6, S-1..S-4) from
  ``invariant_rules.json`` with typed evidence-id / document-anchor /
  open-question citations validated in their own namespaces;
- the H-7 artifact-override rule from the ``HARDENING-SPEC.md`` §2.1
  table, with its three enforcement-layer states and three Rust
  projection-seam symbol locators (names verified, Rust bodies never
  emitted);
- the EV records, boundary classes, fixture arms, request expectations,
  adjudications, and C1–C7 from the single FORMALISM §2 category table;
- the current and retained-superseded H-2/H-5 clause entities joined by
  ``supersedes`` edges, both generations locatored into §2.2a;
- curated heading/question sections (every catalog selector plus cited
  sections) with UTF-8-safe, byte-capped snippets;
- the generated-rule artifacts, coupled to the generator through the
  pure ``derive()`` seam only (stdlib ``importlib`` load with
  ``sys.dont_write_bytecode`` forced; the generator CLI and the linter
  are never invoked or imported during ``build``).

The adapter is read-only: it creates and modifies no corpus byte,
including any ``__pycache__`` entry. Stable IDs are namespaced
(``rule:``, ``evidence:``, ``boundary:``, ``fixture:``,
``request:<fixture-id>:<NNN>``, ``adjudication:``,
``section:<root>:<path>:<selector-hash>``, ``artifact:<root>:<path>``);
any ID collision is a hard finding and the second record is dropped.
"""
from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import re
import sys
from pathlib import Path
from typing import Any, Iterable, Mapping

from smoke.eval_harness import paths
from smoke.eval_harness.contract import (
    AdapterContext,
    AdapterResult,
    ContractError,
    canonical_locator_key,
    sensitive_matches,
)

__all__ = [
    "GENERATOR_MODULE_NAME",
    "H7_RUST_SYMBOLS",
    "NATIVE_KINDS",
    "SelectorError",
    "SNIPPET_CAP_BYTES",
    "build_snippet",
    "discover",
    "extract_heading_range",
    "extract_question_range",
]

#: Component owning the linter/generator/rules sources (catalog id).
COMPONENT_LINTER = "formalism-linter"
#: Component owning the curated corpus documents and fixtures (catalog id).
COMPONENT_CORPUS = "parity-formalism"

# Design-pinned relative corpus paths (always rooted through the supplied
# AdapterContext roots; no absolute machine paths in product code).
TOOLS_DIR = "parity-formalism/tools"
RULES_JSON = f"{TOOLS_DIR}/invariant_rules.json"
EXPECTED_VERDICTS_JSON = f"{TOOLS_DIR}/expected_verdicts_ev12.json"
GENERATOR_PY = f"{TOOLS_DIR}/generate_outbound_lint_rules.py"
LINTER_PY = f"{TOOLS_DIR}/invariant_lint.py"
FORMALISM_MD = "parity-formalism/FORMALISM.md"
HARDENING_MD = "parity-formalism/HARDENING-SPEC.md"
QA_MD = "parity-formalism/Q-A.md"
EXEMPLARS_MD = "parity-formalism/EXEMPLARS.md"
OBSERVATIONS_MD = "parity-formalism/OBSERVATIONS.md"
INTEL_02_MD = "parity-formalism/intel/02-qwen-codexfam-reasoning-parity.md"
FIXTURES_DIR = f"{TOOLS_DIR}/fixtures"
CRATE = "crates/codegen/xai-grok-sampling-types"
RULES_GENERATED_RS = f"{CRATE}/src/conversation/rules_generated.rs"
PROJECTION_TESTS_RS = f"{CRATE}/src/conversation/projection_tests.rs"
HARD_RULES_JSON = f"{CRATE}/fixtures/outbound_lint/hard_rules.json"

#: The §2.1/§2.2a HARDENING-SPEC headings the H-2/H-5 generations and the
#: H-7 row live under (exact catalog selector texts, DESIGN.md §3).
HEADING_2_1 = (
    "2.1 Hard invariants (I^h; default artifact set A1 + A2 + A3 "
    "— a row may override, see H-7)"
)
HEADING_2_2A = (
    "2.2a Strict-row enc re-scope — exact clauses "
    "(T8 OQ-T8-1/2 adjudication, 2026-09-23; bead apex-ayl.126.8.5)"
)

#: The exact native ``meta.json.kind`` enum (DESIGN.md §3); values are
#: never rewritten or collapsed.
NATIVE_KINDS = (
    "verbatim-copy",
    "document-sourced",
    "record-sourced-stub",
    "synthesized",
)

#: H-7's A3 enforcement: the three Rust projection-seam test symbols
#: (DESIGN.md §3). Only the bare names are recorded.
H7_RUST_SYMBOLS = (
    "xw_proj_orphaned_result_direction",
    "xw_proj_surviving_call_keeps_result",
    "xw_proj_carrier_survival",
)

#: Import name under which the generator module is loaded for the pure
#: ``derive()`` seam (stdlib importlib; never the generator CLI).
GENERATOR_MODULE_NAME = "eval_harness_formalism_generator"

SNIPPET_CAP_BYTES = 65_536

#: Document-citation namespaces: first token of the citation -> plans-root
#: relative document (DESIGN.md §3: "EXEMPLARS 5" etc. remain document
#: citations, never relabeled as EV ids).
_CITE_DOCS = {
    "EXEMPLARS": EXEMPLARS_MD,
    "OBSERVATIONS": OBSERVATIONS_MD,
    "intel/02": INTEL_02_MD,
}

_EV_ID_RE = re.compile(r"EV-\d+")
_OQ_ID_RE = re.compile(r"OQ-[A-Za-z0-9]+")
_CITE_DOC_RE = re.compile(r"(EXEMPLARS|OBSERVATIONS|intel/02)\s+(\(?\w+\)?)")
_HEADING_LINE_RE = re.compile(r"^(#{1,6}) (.+)$")
_Q_MARKER_RE = re.compile(r"\*\*Q:(.+?)\*\*", re.DOTALL)
# Absolute path token: leading "/", two or more path segments, and a left
# boundary that is not itself a path character (so "tools/fixtures/..." or
# "A/B/C" never match mid-token).
_ABS_TOKEN_RE = re.compile(
    r"(?<![\w.%/-])(/[A-Za-z0-9._%-]+(?:/[A-Za-z0-9._%-]+)+/?)")
# GFM table rows may be indented 0-3 spaces; 4+ leading spaces (or a
# leading tab) is a code block, not a row, and stays prose (round 7, M-2).
_TABLE_INDENT = r"^ {0,3}"
_CATEGORY_ROW_RE = re.compile(
    _TABLE_INDENT + r"\|\s*(C\d+)\s*\|([^|]*)\|([^|]*)\|([^|]*)\|\s*$")
# C-row prefix for the malformed-row gate (round 6, MAJOR-B): a row whose
# first cell is a C-id but which fails the exact four-column shape.
_CATEGORY_ROW_PREFIX_RE = re.compile(_TABLE_INDENT + r"\|\s*C\d+\s*\|")
# H-row gate (round 6, MAJOR-B; round 7, M-2/M-3; round 8, M-2): a
# table row whose first cell is an H-rule id. The post-pipe padding
# tolerance is the C-row's own (``\s*``): a GFM-legal ``|  H-7 | …``
# row (2-3 spaces after the opening pipe) is a row, not prose — the
# two gates were asymmetric while the H-row gate admitted 0-1 spaces.
# The no-space ``|H-`` variant is included per the coordinator fold of
# the workhorse round-7 observation.
_H_ROW_RE = re.compile(_TABLE_INDENT + r"\|\s*H-")


def _catalog_spec_escapes(spec: str) -> bool:
    """Narrow shape gate for catalog-declared path specs (round 7, M-4a).

    Only an absolute spec (leading ``/``) or a ``..`` path segment is
    rejected. This is deliberately NOT full posix validation: the live
    catalog legitimately declares ``**`` glob specs, which must keep
    flowing to the collector. Schema validation in ``load_catalog`` is
    the primary gate for well-formed catalogs; this keeps a
    schema-violating value from becoming a raw id-mint or host read.
    """
    return spec.startswith("/") or ".." in spec.split("/")


def _catalog_spec_shape_violation(spec: str) -> bool:
    """Narrow shape gate for the path part of a catalog-declared spec
    (round 8, MINOR-1; widened round 14, GLM M-2, to match the
    inventory mirror): an empty/whitespace-only part, ANY whitespace
    character anywhere (round 15, GLM F-1: the schema path-token
    character class ``^[A-Za-z0-9._*%-]+(?:/[A-Za-z0-9._*%-]+)*$``
    forbids whitespace everywhere, so a padded or internal-space body
    violates exactly like an empty one), a ``.`` segment at ANY
    position (``./x``, ``a/./b``, ``a/.``), or any empty segment
    (``a//b``) never mints an artifact id. Deliberately NOT full posix
    validation: the live catalog legitimately declares ``**`` glob specs
    and ``a..b``-substring names, which must keep flowing; absolute and
    ``..`` segments are the M-4a gate's.
    """
    if not spec.strip() or any(ch.isspace() for ch in spec):
        return True
    segments = spec.split("/")
    return "." in segments or "" in segments


def _locator_key_tolerant(source: Mapping[str, Any] | None) -> bytes:
    """Tolerant locator key for stable ordering (round 9, M-6).

    ``finding()`` and the final relationship/finding sorts key on this:
    a locator that ``finding()`` accepts (its key falls back to the
    canonical empty object) can never make a sort raise ``ContractError``
    out of ``discover()``.
    """
    try:
        return canonical_locator_key(source)
    except ContractError:
        return b"{}"


_OQ_ITEM_RE = re.compile(r"^-\s*(OQ-[A-Za-z0-9]+):")
_PIN_RE = re.compile(r"`([^`]+?\.md)(?::(\d+))?`")

#: Sentinel returned by ``_State.read_json`` when a file cannot be read or
#: decoded, so a successful parse of JSON ``null`` (the value ``None``) is
#: never conflated with read failure (DESIGN.md §8: malformed data never
#: silently disappears; round 5, m5).
_READ_FAILED = object()

#: Minted stable-id shape: an ASCII slug. A value minted into an entity id
#: or a relationship target must match this (and pass the sensitive scan)
#: or it is dropped with a hard finding (DESIGN.md §3: never a raw emit).
_ID_RE = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9._-]*$")
_REQ_FILE_RE = re.compile(r"req-(\d{3})\.json")
_SUPERSEDED_MARK_RE = re.compile(r"\[SUPERSEDED-INTERIM[^\]]*\]")
_OVERRIDE_RE = re.compile(r"ARTIFACT OVERRIDE:\s*(.+)$")
_OVERRIDE_LAYERS_RE = re.compile(r"A1 = (.*?);\s*A2 = (.*?);\s*A3 = (.*?)(?:;|\s*)$")


class SelectorError(Exception):
    """A curated heading/question selector matched zero or multiple ranges.

    ``reason`` is exactly ``"missing"`` or ``"duplicate"``.
    """

    def __init__(self, reason: str, selector: str) -> None:
        super().__init__(f"selector {reason}: {selector!r}")
        self.reason = reason
        self.selector = selector


def extract_heading_range(text: str, exact_heading: str) -> str:
    """Return the range from the heading line through the line before the
    next heading line of ANY level (or end of text).

    The heading text must equal ``exact_heading`` byte-for-byte after the
    single space following the ``#`` run; leading or trailing whitespace in
    the candidate is a non-match. Requires exactly one match; zero or more
    than one is a :class:`SelectorError` (``missing`` / ``duplicate``).
    """
    lines = text.splitlines(keepends=True)

    def _heading(line: str) -> str | None:
        match = _HEADING_LINE_RE.match(line.rstrip("\r\n"))
        return match.group(2) if match else None

    matches = [
        i for i, line in enumerate(lines) if _heading(line) == exact_heading
    ]
    if not matches:
        raise SelectorError("missing", exact_heading)
    if len(matches) > 1:
        raise SelectorError("duplicate", exact_heading)
    start = matches[0]
    end = len(lines)
    for j in range(start + 1, len(lines)):
        if _heading(lines[j]) is not None:
            end = j
            break
    segment = lines[start:end]
    while segment and segment[-1].strip() == "":
        segment.pop()
    return "".join(segment)


def extract_question_range(text: str, normalized_marker: str) -> str:
    """Return the range for one Q-A question block.

    A block starts at the line holding the bold ``**Q:`` marker and extends
    through the line before the next ``**Q:`` marker line (or end of text).
    Markers may span wrapped lines: the marker content between ``**Q:`` and
    its closing ``**`` is compared with all internal whitespace collapsed to
    single spaces, so ``normalized_marker`` is the collapsed form. Trailing
    blank lines before the next marker are excluded. Requires exactly one
    match; otherwise :class:`SelectorError` (``missing`` / ``duplicate``).
    """

    def _collapse(value: str) -> str:
        return re.sub(r"\s+", " ", value)

    spans = [
        (match.start(), _collapse("Q:" + match.group(1)))
        for match in _Q_MARKER_RE.finditer(text)
    ]
    matches = [start for start, norm in spans if norm == normalized_marker]
    if not matches:
        raise SelectorError("missing", normalized_marker)
    if len(matches) > 1:
        raise SelectorError("duplicate", normalized_marker)
    start_off = matches[0]
    later = [start for start, _norm in spans if start > start_off]
    end_off = later[0] if later else len(text)
    lines = text.splitlines(keepends=True)
    pos = 0
    start_line = len(lines)
    for i, line in enumerate(lines):
        if pos <= start_off < pos + len(line):
            start_line = i
            break
        pos += len(line)
    if end_off >= len(text):
        end_line = len(lines)
    else:
        end_line = len(lines)
        pos = 0
        for i, line in enumerate(lines):
            if pos <= end_off < pos + len(line):
                end_line = i
                break
            pos += len(line)
    segment = lines[start_line:end_line]
    while segment and segment[-1].strip() == "":
        segment.pop()
    return "".join(segment)


def build_snippet(
    text: str, *, source_sha256: str, locator: Mapping[str, Any]
) -> dict:
    """UTF-8-safe snippet capped at :data:`SNIPPET_CAP_BYTES`.

    ``bytes`` is always the ORIGINAL UTF-8 byte count of ``text``. When the
    cap is exceeded the cut lands on the cap and any trailing partial
    code point is dropped, so ``text`` always round-trips as UTF-8. The
    snippet exposes ``truncated``, the original byte count, the whole-source
    SHA-256, and the typed rooted locator.
    """
    raw = text.encode("utf-8")
    truncated = len(raw) > SNIPPET_CAP_BYTES
    if not truncated:
        out = text
    else:
        out = raw[:SNIPPET_CAP_BYTES].decode("utf-8", "ignore")
    return {
        "text": out,
        "truncated": truncated,
        "bytes": len(raw),
        "source_sha256": source_sha256,
        "locator": dict(locator),
    }


def _load_generator_seam(module_path: Path):
    """Load the generator module for the pure ``derive()`` seam.

    Standard-library ``importlib`` only. ``sys.dont_write_bytecode`` is
    forced for the duration of the exec and restored in ``finally``, so no
    ``__pycache__`` entry is ever created next to the corpus file. The
    module ``__name__`` is the seam name (not ``__main__``), so the
    generator CLI is never invoked, and nothing here imports the linter.
    """
    spec = importlib.util.spec_from_file_location(
        GENERATOR_MODULE_NAME, str(module_path))
    if spec is None or spec.loader is None:
        raise ContractError(
            f"cannot build an import spec for {module_path}", code="generator-seam")
    module = importlib.util.module_from_spec(spec)
    sys.modules[GENERATOR_MODULE_NAME] = module
    previous = sys.dont_write_bytecode
    sys.dont_write_bytecode = True
    try:
        spec.loader.exec_module(module)
    finally:
        sys.dont_write_bytecode = previous
        # Leave no process-visible seam residue behind discover().
        sys.modules.pop(GENERATOR_MODULE_NAME, None)
    return module


def _layer_state(layer: str, fragment: str) -> str | None:
    """Map one H-7 ARTIFACT OVERRIDE fragment to its design-pinned state."""
    frag = fragment.strip()
    if layer == "A1" and frag.startswith("T8 only") and "EV-12" in frag:
        return "excluded-pending-T8-EV-12"
    if layer == "A2" and frag.startswith("n/a") and "history shape" in frag:
        return "not-applicable-history-shape"
    if layer == "A3" and "projection-seam test" in frag:
        return "enforced-by-rust-projection-seam-test"
    return None


class _State:
    """Discovery state: entity/relationship/finding registries."""

    def __init__(self, ctx: AdapterContext) -> None:
        self.roots: Mapping[str, Path] = ctx.roots
        self.catalog: Mapping[str, Any] = ctx.catalog
        self.entities: dict[str, dict] = {}
        self._rel_records: list[dict] = []
        self._rel_ids: set[str] = set()
        self.findings: list[dict] = []
        self._finding_seq: dict[tuple, int] = {}
        self.oq_registry: set[str] = set()
        self.oq_section_id: str | None = None
        self.oq_section_heading: str | None = None
        self.boundary_map: dict | None = None
        self.rules_ok = False
        self._doc_cache: dict[tuple, tuple] = {}

    # -- registries ---------------------------------------------------------

    def entity(self, record: Mapping[str, Any]) -> None:
        eid = record.get("id")
        if not isinstance(eid, str) or not eid:
            return
        if eid in self.entities:
            source = record.get("source")
            if not isinstance(source, Mapping) or not source.get("root"):
                source = {"root": "plans", "path": RULES_JSON}
            self.finding("formalism-id-collision", dict(source))
            return
        self.entities[eid] = dict(record)

    def rel(
        self,
        kind: str,
        source: str,
        target: str,
        locator: Mapping[str, Any] | None = None,
    ) -> None:
        key = canonical_locator_key(locator)
        rid = (
            f"{kind}::{source}::{target}::"
            f"{hashlib.sha256(key).hexdigest()[:8]}"
        )
        if rid in self._rel_ids:
            return
        self._rel_ids.add(rid)
        record = {"id": rid, "kind": kind, "source": source, "target": target}
        if locator:
            record["locator"] = dict(locator)
        self._rel_records.append(record)

    def finding(
        self,
        code: str,
        source: Mapping[str, Any],
        *,
        component: str = COMPONENT_CORPUS,
        level: str = "error",
        impact: str = "hard",
        **extra: Any,
    ) -> None:
        key = _locator_key_tolerant(source)
        seq = self._finding_seq.get((component, code, key), 0)
        self._finding_seq[(component, code, key)] = seq + 1
        record = {
            "id": (
                f"finding::{component}::{code}::"
                f"{hashlib.sha256(key).hexdigest()[:8]}::{seq:02d}"
            ),
            "code": code,
            "component": component,
            "level": level,
            "impact": impact,
            "source": dict(source),
            "occurrence": seq,
        }
        record.update(extra)
        self.findings.append(record)

    # -- source access ------------------------------------------------------

    def read(self, root_id: str, rel: str) -> bytes | None:
        if root_id not in self.roots:
            return None
        try:
            return (Path(self.roots[root_id]) / rel).read_bytes()
        except OSError:
            return None

    def read_json(self, root_id: str, rel: str, component: str) -> Any:
        """Parsed document, or the ``_READ_FAILED`` sentinel with a hard
        ``formalism-malformed-source`` finding. A successful parse of
        JSON ``null`` returns ``None`` — distinguishable from read
        failure (round 5, m5)."""
        data = self.read(root_id, rel)
        if data is None:
            self.finding(
                "formalism-malformed-source",
                {"root": root_id, "path": rel}, component=component)
            return _READ_FAILED
        try:
            return json.loads(data.decode("utf-8"))
        except (UnicodeDecodeError, ValueError):
            self.finding(
                "formalism-malformed-source",
                {"root": root_id, "path": rel}, component=component)
            return _READ_FAILED

    def doc(self, root_id: str, rel: str) -> tuple[str | None, str | None]:
        """(text, whole-file sha256) or (None, None) for an unreadable
        or non-UTF-8 doc (hard ``formalism-malformed-source``)."""
        key = (root_id, rel)
        if key not in self._doc_cache:
            data = self.read(root_id, rel)
            if data is None:
                self._doc_cache[key] = (None, None)
            else:
                try:
                    text = data.decode("utf-8")
                except UnicodeDecodeError:
                    self.finding(
                        "formalism-malformed-source",
                        {"root": root_id, "path": rel})
                    self._doc_cache[key] = (None, None)
                else:
                    self._doc_cache[key] = (
                        text,
                        hashlib.sha256(data).hexdigest(),
                    )
        return self._doc_cache[key]

    def resolve_root_for(self, rel: str) -> str | None:
        """The first supplied root (sorted) holding ``rel`` as a file.

        Only clean relative tokens are probed: an absolute or ``..``
        token is refused rather than joined — pathlib drops the root
        for an absolute token and would probe an arbitrary host path.
        """
        if not isinstance(rel, str) or not rel:
            return None
        if rel.startswith("/") or ".." in rel:
            return None
        for root_id in sorted(self.roots):
            try:
                if (Path(self.roots[root_id]) / rel).is_file():
                    return root_id
            except OSError:
                continue
        return None

    # -- text hygiene -------------------------------------------------------

    def _norm_token(self, token: str, loc: Mapping[str, Any],
                    component: str) -> str:
        try:
            return paths.normalize_path_token(token, self.roots)
        except ContractError:
            base = token.rstrip("/").rsplit("/", 1)[-1]
            self.finding("formalism-path-alias", loc, component=component)
            return (
                f"@external/{base}#"
                f"{hashlib.sha256(token.encode('utf-8')).hexdigest()}"
            )

    def rewrite_absolute(self, text: str, loc: Mapping[str, Any],
                         component: str) -> str:
        def _sub(match: "re.Match[str]") -> str:
            return self._norm_token(match.group(0), loc, component)

        return _ABS_TOKEN_RE.sub(_sub, text)

    def safe_text(self, text: str, source: Mapping[str, Any],
                  component: str) -> str | None:
        """Normalize absolute tokens, then reject sensitive content.

        Returns the normalized text, or None (with a hard
        ``formalism-sensitive-output`` finding) when a pinned raw-key or
        canary pattern matches. The matched text is never echoed: only
        pattern names and offsets are recorded. A present-but-empty
        string is a hard ``formalism-malformed-source`` at the caller's
        locator (stated choice, round 5, m10: reject, not accept).
        """
        if not isinstance(text, str):
            return None
        if not text:
            self.finding("formalism-malformed-source", dict(source),
                         component=component)
            return None
        out = self.rewrite_absolute(text, source, component)
        hits = sensitive_matches(out)
        if hits:
            self.finding(
                "formalism-sensitive-output", source, component=component,
                detail=[{"pattern": h["pattern"], "offset": h["offset"]}
                        for h in hits])
            return None
        return out

    def locator_channel(self, token: str, loc: Mapping[str, Any],
                        component: str) -> dict | None:
        """Validate one structured locator token (M1).

        Free text flows through ``safe_text``, which rewrites absolute
        tokens in place. A structured locator channel (H-7 code pins,
        boundary source files) has no text fallback: a token that is an
        absolute path or carries any ``..`` (the schemas' substring
        rule) is refused with a hard ``formalism-path-alias`` finding
        and never copied raw. An absolute token that maps beneath a
        supplied root is still emitted, but only in its rooted
        ``{root, path}`` form. Every emitted path — relative token or
        rooted relative part — passes the sensitive scan: a hit is a
        hard ``formalism-sensitive-output`` finding and the token is
        dropped (coordinator decision, round 5, m8). Returns
        ``{"path": ...}`` (owning root resolved by the caller) or
        ``{"root": ..., "path": ...}`` (absolute token, lexical root
        match), or None when dropped.
        """
        if not isinstance(token, str) or not token:
            self.finding("formalism-path-alias", loc, component=component)
            return None
        if token.startswith("/"):
            try:
                norm = paths.normalize_path_token(token, self.roots)
            except ContractError:
                norm = None
            self.finding("formalism-path-alias", loc, component=component)
            if norm is not None and norm.startswith("@"):
                root_tok, _, rel = norm[1:].partition("/")
                if (
                    root_tok in self.roots
                    and rel
                    and not rel.startswith("/")
                    and ".." not in rel
                ):
                    hits = sensitive_matches(rel)
                    if hits:
                        self.finding(
                            "formalism-sensitive-output", loc,
                            component=component,
                            detail=[{"pattern": h["pattern"],
                                     "offset": h["offset"]}
                                    for h in hits])
                        return None
                    return {"root": root_tok, "path": rel}
            return None
        if (
            ".." in token
            or token.startswith("./")
            or "//" in token
            or "." in token.split("/")
        ):
            self.finding("formalism-path-alias", loc, component=component)
            return None
        try:
            norm = paths.normalize_path_token(token, self.roots)
        except ContractError:
            self.finding("formalism-path-alias", loc, component=component)
            return None
        hits = sensitive_matches(norm)
        if hits:
            # The relative branch previously had no sensitive scan; the
            # raw token is never echoed into the finding locator.
            self.finding(
                "formalism-sensitive-output", loc, component=component,
                detail=[{"pattern": h["pattern"], "offset": h["offset"]}
                        for h in hits])
            return None
        return {"path": norm}

    # -- field hygiene (round 4: the stable-record rule per field family) ----

    def _safe_id(self, value: Any, loc: Mapping[str, Any],
                 component: str) -> str | None:
        """One value minted into an entity id or relationship target.

        Non-string/empty and ID-shape failures are hard
        ``formalism-malformed-source``; sensitive or absolute-token
        content is refused through :meth:`safe_text`. A rejected value
        mints no id and rides into no relationship. The raw value never
        reaches the finding locator.
        """
        if not isinstance(value, str) or not value:
            self.finding("formalism-malformed-source", dict(loc),
                         component=component)
            return None
        safe = self.safe_text(value, dict(loc), component)
        if safe is None:
            return None
        if not _ID_RE.fullmatch(safe):
            self.finding("formalism-malformed-source", dict(loc),
                         component=component)
            return None
        return safe

    def _safe_field(self, value: Any, loc: Mapping[str, Any],
                    component: str) -> str | None:
        """One copied text field: full :meth:`safe_text` (absolute tokens
        rewritten in place, sensitive content a hard finding with the
        field dropped). A present non-string value is a hard
        ``formalism-malformed-source`` finding, never a silent skip."""
        if not isinstance(value, str):
            self.finding("formalism-malformed-source", dict(loc),
                         component=component)
            return None
        return self.safe_text(value, dict(loc), component)

    def _scan_only(self, value: Any, loc: Mapping[str, Any],
                   component: str) -> str | None:
        """Sensitive scan without absolute-token rewriting.

        For document text where absolute-looking tokens are data (wire
        routes in request paths, route mentions in headings) per the
        coordinator adjudication: a hit is a hard
        ``formalism-sensitive-output`` finding and the value is rejected;
        a present non-string value is a hard malformed finding; a
        present-but-empty string is a hard
        ``formalism-malformed-source`` at the caller's locator, value
        dropped (mirrors :meth:`safe_text`; round 6, m-1); clean text is
        returned verbatim.
        """
        if not isinstance(value, str):
            self.finding("formalism-malformed-source", dict(loc),
                         component=component)
            return None
        if not value:
            self.finding("formalism-malformed-source", dict(loc),
                         component=component)
            return None
        hits = sensitive_matches(value)
        if hits:
            self.finding(
                "formalism-sensitive-output", dict(loc),
                component=component,
                detail=[{"pattern": h["pattern"], "offset": h["offset"]}
                        for h in hits])
            return None
        return value

    def _int_field(self, value: Any, loc: Mapping[str, Any],
                   component: str) -> int | None:
        """One expected-count field: an int (bool excluded) or a hard
        ``formalism-malformed-source`` finding with the value dropped."""
        if not isinstance(value, int) or isinstance(value, bool):
            self.finding("formalism-malformed-source", dict(loc),
                         component=component)
            return None
        return value

    def _finding_list(self, raw: Any, loc: Mapping[str, Any],
                      component: str) -> list:
        """One hard/soft finding-list field: a list of string entries,
        each safe-texted. A non-list container is ONE hard malformed
        finding (never iterated); a non-list entry or a bad string is a
        hard finding and the whole entry is dropped."""
        if raw is None:
            return []
        if not isinstance(raw, list):
            self.finding("formalism-malformed-source", dict(loc),
                         component=component)
            return []
        out = []
        for j, entry in enumerate(raw):
            entry_loc = {**loc, "key": f"{loc.get('key')}[{j}]"}
            if not isinstance(entry, list):
                self.finding("formalism-malformed-source", entry_loc,
                             component=component)
                continue
            cleaned = []
            ok = True
            for k, s in enumerate(entry):
                if not isinstance(s, str) or not s:
                    self.finding("formalism-malformed-source",
                                 {**entry_loc, "entry": k},
                                 component=component)
                    ok = False
                    break
                safe = self.safe_text(s, {**entry_loc, "entry": k},
                                      component)
                if safe is None:
                    ok = False
                    break
                cleaned.append(safe)
            if ok:
                out.append(cleaned)
        return out

    # -- sections -----------------------------------------------------------

    def _snippet(self, rng: str, sha: str, loc: Mapping[str, Any]) -> dict:
        safe = self.safe_text(rng, loc, COMPONENT_CORPUS)
        return build_snippet(
            "" if safe is None else safe, source_sha256=sha, locator=dict(loc))

    def _section_entity(self, root_id: str, rel: str, kind: str, text_: str,
                        rng: str, sha: str,
                        loc: Mapping[str, Any]) -> str | None:
        # The selector text is document text (headings may carry wire
        # routes): scan-only, never rewritten, and the raw selector is
        # never echoed into the finding locator. A sensitive selector is a
        # hard finding and the section is not registered.
        if self._scan_only(text_, {"root": root_id, "path": rel},
                           COMPONENT_CORPUS) is None:
            return None
        sec_id = (
            f"section:{root_id}:{rel}:"
            f"{hashlib.sha256(text_.encode('utf-8')).hexdigest()}"
        )
        if sec_id in self.entities:
            return sec_id
        selector = {"kind": kind}
        selector["text" if kind == "heading" else "marker"] = text_
        self.entity({
            "id": sec_id,
            "kind": "document-section",
            "root": root_id,
            "path": rel,
            "selector": selector,
            "snippet": self._snippet(rng, sha, loc),
            "source": dict(loc),
        })
        return sec_id

    def parse_curated_sections(self) -> None:
        catalog_loc = {"root": "worktree",
                       "path": "smoke/eval_harness/catalog.json",
                       "key": "curated_sources"}
        # Container discipline (round 10, MAJOR-3): None is a clean
        # empty walk (today's behavior); a non-list container is ONE
        # hard finding at the CONTAINER locator (no "index" key) and
        # the walk returns — an int escapes as a raw TypeError and a
        # string char-iterates into per-char garbage findings.
        curated = self.catalog.get("curated_sources")
        if curated is None:
            curated = []
        elif not isinstance(curated, list):
            self.finding("formalism-malformed-source", dict(catalog_loc),
                         component=COMPONENT_CORPUS)
            return
        for i, source in enumerate(curated):
            # Every shape violation in this walk is ONE hard finding at
            # the catalog-file locator (entry index; selector index for
            # the selector violation). Container-level violations
            # (non-dict entry, bad root/path, non-list selectors,
            # non-dict selector) drop the entry; per-selector
            # kind/text violations skip that selector only (round 9,
            # M-1) — a declared source never silently vanishes with its
            # section entities, categories, and defines edges
            # (DESIGN.md §8; round 8, MAJOR-1).
            if not isinstance(source, dict):
                self.finding(
                    "formalism-malformed-source",
                    {**catalog_loc, "index": i},
                    component=COMPONENT_CORPUS)
                continue
            root_id = source.get("root")
            # A non-string root, and a root the context does not supply
            # (the catalog names a root the AdapterContext does not
            # provide — schema-violating), both hard + drop.
            if not isinstance(root_id, str) or root_id not in self.roots:
                self.finding(
                    "formalism-malformed-source",
                    {**catalog_loc, "index": i},
                    component=COMPONENT_CORPUS)
                continue
            rel = source.get("path")
            # A present-but-empty/whitespace-only path is the same hard
            # + drop as a non-string one — pre-fix "" passed both gates
            # and cascaded into per-selector findings (round 9, M-3).
            if not isinstance(rel, str) or not rel.strip():
                self.finding(
                    "formalism-malformed-source",
                    {**catalog_loc, "index": i},
                    component=COMPONENT_CORPUS)
                continue
            if _catalog_spec_escapes(rel):
                # A schema-violating absolute or ``..``-segment catalog
                # path never mints a section id or becomes a host read
                # (DESIGN.md §8; round 7, M-4a).
                self.finding(
                    "formalism-malformed-source",
                    {**catalog_loc, "index": i},
                    component=COMPONENT_CORPUS)
                continue
            # A schema-legal filename carrying a raw key/canary never
            # raw-emits into section ids: ONE hard sensitive finding at
            # the entry locator — same detail/rel_sha256 shape as the
            # artifact-channel scan; the raw name rides along as a
            # hash, never echoed. The entry is dropped, no section ids
            # minted (round 9, M-2).
            hits = sensitive_matches(rel)
            if hits:
                self.finding(
                    "formalism-sensitive-output",
                    {**catalog_loc, "index": i},
                    component=COMPONENT_CORPUS,
                    detail=[{"pattern": h["pattern"], "offset": h["offset"]}
                            for h in hits],
                    rel_sha256=hashlib.sha256(rel.encode("utf-8")).hexdigest())
                continue
            # A dot-leading (./x) or empty-segment (a//b) curated path
            # never mints a section id — the same helper the two
            # input-walk branches use (round 9, M-4).
            if _catalog_spec_shape_violation(rel):
                self.finding(
                    "formalism-malformed-source",
                    {**catalog_loc, "index": i},
                    component=COMPONENT_CORPUS)
                continue
            # A NUL passes every shape gate above and then escapes
            # ``read()`` as a raw ``ValueError: embedded null byte``:
            # one hard at the entry locator, entry dropped, raw value
            # never echoed (round 15, QC M-1; mirrors the inventory
            # _reject_nul-at-every-channel discipline).
            if "\x00" in rel:
                self.finding(
                    "formalism-malformed-source",
                    {**catalog_loc, "index": i},
                    component=COMPONENT_CORPUS)
                continue
            selectors = source.get("selectors")
            if selectors is None:
                selectors = []
            if not isinstance(selectors, list):
                # A present-but-non-list container is never iterated (a
                # string char-iterates into silent skips): one hard
                # finding at the entry locator, entry dropped. Absent
                # and None are clean (round 8, MAJOR-1).
                self.finding(
                    "formalism-malformed-source",
                    {**catalog_loc, "index": i},
                    component=COMPONENT_CORPUS)
                continue
            for j, sel in enumerate(selectors):
                if not isinstance(sel, dict):
                    # Non-dict selector: one hard finding at the entry
                    # locator plus the selector index, and the entry is
                    # dropped — no further selectors of the entry are
                    # processed (round 8, MAJOR-1).
                    self.finding(
                        "formalism-malformed-source",
                        {**catalog_loc, "index": i, "selector": j},
                        component=COMPONENT_CORPUS)
                    break
                kind = sel.get("kind")
                if kind == "citations":
                    # Live constraint: the live catalog pins kind
                    # "citations" on EXEMPLARS.md, OBSERVATIONS.md, and
                    # intel/02 — those sections resolve through the
                    # citation-driven path, never this walk. Keep the
                    # silent continue exactly as-is (round 9, M-1).
                    continue
                if kind not in ("heading", "question"):
                    # Missing or unknown kind: ONE hard finding at the
                    # selector locator and skip that selector only —
                    # the entry stays alive, remaining selectors are
                    # processed (round 9, M-1).
                    self.finding(
                        "formalism-malformed-source",
                        {**catalog_loc, "index": i, "selector": j},
                        component=COMPONENT_CORPUS)
                    continue
                text_ = sel.get("text") if kind == "heading" else sel.get("marker")
                if not isinstance(text_, str) or not text_.strip():
                    # Non-string or empty/whitespace-only text/marker:
                    # the same single hard finding at the selector
                    # locator, selector skipped, document never read
                    # (round 9, M-1; whitespace-only gated here, not at
                    # a loud selector-missing after reading the
                    # document — round 10, MINOR-2, symmetric with the
                    # curated rel.strip() discipline).
                    self.finding(
                        "formalism-malformed-source",
                        {**catalog_loc, "index": i, "selector": j},
                        component=COMPONENT_CORPUS)
                    continue
                hits = sensitive_matches(text_)
                if hits:
                    # Catalog-declared selector text is scanned before
                    # any locator is built or document read: a raw
                    # key/canary in the text never rides along into a
                    # finding source on ANY path (round 10, MAJOR-1).
                    # Non-echoing catalog-file locator; the raw value
                    # rides along as a hash.
                    self.finding(
                        "formalism-sensitive-output",
                        {**catalog_loc, "index": i, "selector": j},
                        component=COMPONENT_CORPUS,
                        detail=[{"pattern": h["pattern"],
                                 "offset": h["offset"]} for h in hits],
                        value_sha256=hashlib.sha256(
                            text_.encode("utf-8")).hexdigest())
                    continue
                loc_key = "heading" if kind == "heading" else "question"
                loc = {"root": root_id, "path": rel, loc_key: text_}
                doc_text, sha = self.doc(root_id, rel)
                if doc_text is None or sha is None:
                    self.finding("formalism-selector-missing", loc)
                    continue
                try:
                    if kind == "heading":
                        rng = extract_heading_range(doc_text, text_)
                    else:
                        rng = extract_question_range(doc_text, text_)
                except SelectorError as exc:
                    code = (
                        "formalism-selector-missing"
                        if exc.reason == "missing"
                        else "formalism-selector-duplicate"
                    )
                    self.finding(code, loc)
                    continue
                sec_id = self._section_entity(root_id, rel, kind, text_, rng, sha, loc)
                if sec_id is None:
                    continue
                self._post_section(root_id, rel, kind, text_, rng, sec_id, loc)

    def _post_section(self, root_id: str, rel: str, kind: str, text_: str,
                      rng: str, sec_id: str, loc: Mapping[str, Any]) -> None:
        if kind != "heading":
            return
        if rel == FORMALISM_MD and text_.startswith("2. "):
            self._parse_categories(rng, sec_id, loc)
            return
        if rel != HARDENING_MD:
            return
        if text_.startswith("2.1 "):
            self._parse_hardening_table(rng, loc)
        elif text_.startswith("2.3 "):
            self.oq_section_id = sec_id
            self.oq_section_heading = text_
            self.oq_registry = set(re.findall(_OQ_ITEM_RE.pattern, rng, re.M))

    def _parse_categories(self, rng: str, sec_id: str,
                          loc: Mapping[str, Any]) -> None:
        for i, line in enumerate(rng.splitlines()):
            match = _CATEGORY_ROW_RE.match(line)
            if match is None and _CATEGORY_ROW_PREFIX_RE.match(line):
                # A row shaped as a C-category row but failing the exact
                # four-column form (e.g. an extra column) never silently
                # disappears: hard finding at the non-echoing row locator
                # (document root + row index; the row text is never
                # echoed), then the row is skipped (DESIGN.md §8; round
                # 6, MAJOR-B). Header/separator/prose rows match neither
                # regex and stay silently skipped.
                self.finding(
                    "formalism-malformed-source",
                    {**dict(loc), "row": i},
                    component=COMPONENT_CORPUS)
                continue
            if match is None:
                continue
            cid, name, what, cost = (g.strip() for g in match.groups())
            ent = {
                "id": f"category:{cid}",
                "kind": "category",
                "category_id": cid,
                "source": dict(loc),
            }
            safe_name = self.safe_text(name, loc, COMPONENT_CORPUS)
            if safe_name is not None:
                ent["name"] = safe_name
            safe_what = self.safe_text(what, loc, COMPONENT_CORPUS)
            if safe_what is not None:
                ent["what_is"] = safe_what
            safe_cost = self.safe_text(cost, loc, COMPONENT_CORPUS)
            if safe_cost is not None:
                ent["cost_channel"] = safe_cost
            self.entity(ent)
            self.rel("defines", sec_id, f"category:{cid}", locator=loc)

    # -- HARDENING-SPEC §2.1 table: H-7 + superseded H-2/H-5 ----------------

    def _parse_hardening_table(self, rng: str, loc: Mapping[str, Any]) -> None:
        for i, line in enumerate(rng.splitlines()):
            if not _H_ROW_RE.match(line):
                continue
            cells = [c.strip() for c in line.strip().strip("|").split("|")]
            if len(cells) != 4:
                # A mis-shaped H-rule row — truncated (<4 cells) OR with
                # an extra column (>4 cells) — never silently
                # disappears: hard finding at the non-echoing row
                # locator (document root + row index; the row text is
                # never echoed), then the row is skipped (DESIGN.md §8;
                # round 6, MAJOR-B; round 7, M-3: the extra-cell
                # direction is symmetric with the C-row gate). Unknown-
                # rid 4-cell rows (the live H-1/H-3/H-4/H-6 owned by
                # invariant_rules.json) intentionally stay ignored.
                self.finding(
                    "formalism-malformed-source",
                    {**dict(loc), "row": i},
                    component=COMPONENT_CORPUS)
                continue
            rid, cls, invariant, evcell = cells[:4]
            if rid in ("H-2", "H-5"):
                self._superseded_clause(rid, invariant, loc)
            elif rid == "H-7":
                self._h7_rule(cls, invariant, evcell, loc, row=i)

    def _superseded_clause(self, rid: str, invariant: str,
                           loc: Mapping[str, Any]) -> None:
        mark = _SUPERSEDED_MARK_RE.search(invariant)
        if mark is None:
            self.finding("formalism-missing-fact", loc)
            return
        clause = self.safe_text(mark.group(0), loc, COMPONENT_CORPUS)
        if clause is None:
            return
        self.entity({
            "id": f"rule:{rid}:superseded",
            "kind": "rule",
            "rule_id": rid,
            "status": "superseded",
            "clause": clause,
            "source": dict(loc),
            "locators": [
                dict(loc),
                {"root": "plans", "path": HARDENING_MD, "heading": HEADING_2_2A},
            ],
        })
        self.rel(
            "supersedes",
            f"rule:{rid}",
            f"rule:{rid}:superseded",
            locator={"root": "plans", "path": HARDENING_MD, "heading": HEADING_2_2A},
        )

    def _h7_rule(self, cls: str, invariant: str, evcell: str,
                 loc: Mapping[str, Any], row: int) -> None:
        override = _OVERRIDE_RE.search(invariant)
        clause = (
            invariant if override is None else invariant[: override.start()]
        ).strip()
        layers: list[dict] = []
        if override is None:
            self.finding("formalism-missing-fact", loc)
        else:
            lm = _OVERRIDE_LAYERS_RE.search(override.group(1))
            if lm is None:
                self.finding("formalism-missing-fact", loc)
            else:
                for layer, frag in (
                    ("A1", lm.group(1)), ("A2", lm.group(2)), ("A3", lm.group(3))
                ):
                    state = _layer_state(layer, frag)
                    if state is None:
                        self.finding("formalism-missing-fact", loc)
                    else:
                        layers.append({"layer": layer, "state": state})
        # ``row`` is the 0-based index of the H-7 row inside the §2.1
        # heading range; it appears ONLY in the non-echoing row locator
        # below — the section-level locators of this method are
        # unchanged (DESIGN.md §8; round 8, MAJOR-2).
        pins: list[dict] = []
        pins_present = bool(evcell)
        if not pins_present:
            # A present-but-empty EV cell never silently vanishes: ONE
            # hard finding at the non-echoing row locator; ``code_pins``
            # is OMITTED (no pins minted, no artifact entity registered
            # from the row) while ``rule:H-7`` is otherwise retained
            # (DESIGN.md §8; round 8, MAJOR-2).
            self.finding(
                "formalism-malformed-source",
                {**dict(loc), "row": row},
                component=COMPONENT_CORPUS)
        for pm in _PIN_RE.finditer(evcell):
            channel = self.locator_channel(
                pm.group(1), loc, COMPONENT_CORPUS)
            if channel is None:
                continue
            # The pin record carries the verified owning root (the
            # channel's lexical root for beneath-root absolute tokens,
            # else the first supplied root holding the file, else the
            # "plans" default).
            pin = {
                "root": (
                    channel.get("root")
                    or self.resolve_root_for(channel["path"])
                    or "plans"
                ),
                "path": channel["path"],
            }
            if pm.group(2):
                pin["line"] = int(pm.group(2))
            pins.append(pin)
        ent = {
            "id": "rule:H-7",
            "kind": "rule",
            "rule_id": "H-7",
            "status": "current",
            "severity": "hard",
            "source": dict(loc),
        }
        if pins_present:
            ent["code_pins"] = pins
        safe_cls = self.safe_text(cls, loc, COMPONENT_CORPUS)
        if safe_cls is not None:
            ent["class"] = safe_cls
        safe_clause = self.safe_text(clause, loc, COMPONENT_CORPUS)
        if safe_clause is not None:
            ent["clause"] = safe_clause
        if layers:
            ent["enforcement_layers"] = layers
        self.entity(ent)
        for pin in pins:
            self._register_code_pin(pin)

    def _register_code_pin(self, pin: Mapping[str, Any]) -> None:
        rel = pin["path"]
        if not isinstance(rel, str) or rel.startswith("/") or ".." in rel:
            # Refused upstream by the locator channel; refuse again so no
            # host path is ever probed or emitted.
            self.finding(
                "formalism-path-alias",
                {"root": pin.get("root") or "plans", "path": str(rel)},
            )
            return
        resolved = self.resolve_root_for(rel)
        if resolved is None:
            self.finding(
                "formalism-missing-fact",
                {"root": pin["root"], "path": rel},
                level="warning", impact="soft")
            return
        root_id = resolved
        if "line" in pin:
            data = self.read(root_id, rel)
            line_count = (
                len(data.decode("utf-8", "replace").split("\n"))
                if data is not None else 0
            )
            if line_count < pin["line"]:
                self.finding(
                    "formalism-missing-fact",
                    {"root": root_id, "path": rel, "line": pin["line"]},
                    level="warning", impact="soft")
        self._register_artifact(root_id, rel, role="evidence-report")

    # -- citations ------------------------------------------------------------

    def resolve_doc_anchor(self, doc_name: str, ref: str,
                           loc: Mapping[str, Any]) -> dict | None:
        rel = _CITE_DOCS[doc_name]
        doc_text, sha = self.doc("plans", rel)
        if doc_text is None or sha is None:
            self.finding(
                "formalism-malformed-source",
                {"root": "plans", "path": rel}, component=COMPONENT_LINTER)
            return None
        prefix = f"{ref} " if ref.startswith("(") else f"{ref}. "
        hits = [
            match.group(2)
            for line in doc_text.splitlines()
            if (match := _HEADING_LINE_RE.match(line.rstrip("\r\n")))
            and match.group(2).startswith(prefix)
        ]
        if len(hits) != 1:
            self.finding(
                "formalism-unknown-citation", loc, component=COMPONENT_LINTER)
            return None
        heading = hits[0]
        locator = {"root": "plans", "path": rel, "heading": heading}
        try:
            rng = extract_heading_range(doc_text, heading)
        except SelectorError:
            self.finding(
                "formalism-unknown-citation", loc, component=COMPONENT_LINTER)
            return None
        sec_id = self._section_entity(
            "plans", rel, "heading", heading, rng, sha, locator)
        if sec_id is None:
            # Sensitive heading: the hard finding is already recorded and
            # the citation is rejected, never copied.
            return None
        return {
            "kind": "document-anchor",
            "root": "plans",
            "path": rel,
            "heading": heading,
        }

    def _citation(self, token: str, registry: Mapping[str, str],
                  loc: Mapping[str, Any]) -> dict | None:
        if _EV_ID_RE.fullmatch(token):
            if token in registry:
                return {"kind": "evidence-id", "id": token}
            self.finding(
                "formalism-unknown-citation", loc, component=COMPONENT_LINTER)
            return None
        dm = _CITE_DOC_RE.fullmatch(token)
        if dm:
            return self.resolve_doc_anchor(dm.group(1), dm.group(2), loc)
        if _OQ_ID_RE.fullmatch(token):
            if token in self.oq_registry:
                return {"kind": "open-question-id", "id": token}
            self.finding(
                "formalism-unknown-citation", loc, component=COMPONENT_LINTER)
            return None
        self.finding(
            "formalism-unknown-citation", loc, component=COMPONENT_LINTER)
        return None

    # -- invariant_rules.json -------------------------------------------------

    def parse_rules(self) -> None:
        doc = self.read_json("plans", RULES_JSON, component=COMPONENT_LINTER)
        if doc is _READ_FAILED:
            # read_json already recorded the hard finding.
            return
        if not isinstance(doc, dict):
            # A valid-JSON non-dict doc (list, string, JSON null) is a
            # hard malformed-source at the file locator, never a silent
            # return (round 5, m1): the drift gate then has no valid
            # input, and the failure stays visible.
            self.finding(
                "formalism-malformed-source",
                {"root": "plans", "path": RULES_JSON},
                component=COMPONENT_LINTER)
            return
        registry: dict[str, str] = {}
        ev_reg = doc.get("ev_registry")
        if ev_reg is not None:
            if not isinstance(ev_reg, dict):
                self.finding(
                    "formalism-malformed-source",
                    {"root": "plans", "path": RULES_JSON,
                     "key": "ev_registry"},
                    component=COMPONENT_LINTER)
            else:
                for i, ev_id in enumerate(sorted(ev_reg)):
                    fact = ev_reg[ev_id]
                    if (not isinstance(ev_id, str)
                            or not _EV_ID_RE.fullmatch(ev_id)
                            or not isinstance(fact, str)):
                        # Shape/safe-text check BEFORE the locator is
                        # built: the raw key never reaches the finding —
                        # only its index in the sorted key list (round 5,
                        # m2).
                        self.finding(
                            "formalism-malformed-source",
                            {"root": "plans", "path": RULES_JSON,
                             "key": "ev_registry", "index": i},
                            component=COMPONENT_LINTER)
                        continue
                    src = {"root": "plans", "path": RULES_JSON, "key": ev_id}
                    safe = self.safe_text(fact, src, COMPONENT_LINTER)
                    if safe is None:
                        continue
                    registry[ev_id] = safe
                    self.entity({
                        "id": f"evidence:{ev_id}",
                        "kind": "evidence-record",
                        "ev_id": ev_id,
                        "fact": safe,
                        "source": src,
                    })
        self._boundary_map(doc)
        rules = doc.get("rules")
        if rules is not None:
            if not isinstance(rules, list):
                self.finding(
                    "formalism-malformed-source",
                    {"root": "plans", "path": RULES_JSON, "key": "rules"},
                    component=COMPONENT_LINTER)
            else:
                for i, rule in enumerate(rules):
                    loc = {"root": "plans", "path": RULES_JSON, "index": i}
                    if not isinstance(rule, dict):
                        self.finding(
                            "formalism-malformed-source", loc,
                            component=COMPONENT_LINTER)
                        continue
                    # The rule id mints the ``rule:{id}`` entity id; a
                    # value failing hygiene is dropped with a hard finding
                    # and mints nothing.
                    safe_rid = self._safe_id(rule.get("id"), loc,
                                             COMPONENT_LINTER)
                    if safe_rid is None:
                        continue
                    cites = []
                    ev_raw = rule.get("ev")
                    if ev_raw is None:
                        pass
                    elif not isinstance(ev_raw, list):
                        self.finding(
                            "formalism-malformed-source",
                            {**loc, "key": "ev"},
                            component=COMPONENT_LINTER)
                    else:
                        for j, token in enumerate(ev_raw):
                            if not isinstance(token, str):
                                self.finding(
                                    "formalism-malformed-source",
                                    {**loc, "key": "ev", "entry": j},
                                    component=COMPONENT_LINTER)
                                continue
                            cite = self._citation(
                                token, registry,
                                {**loc, "key": "ev", "entry": j})
                            if cite is not None:
                                cites.append(cite)
                    ent = {
                        "id": f"rule:{safe_rid}",
                        "kind": "rule",
                        "rule_id": safe_rid,
                        "status": "current",
                        "source": loc,
                        "citations": cites,
                    }
                    for field in ("class", "severity", "check"):
                        value = rule.get(field)
                        if value is None:
                            continue
                        safe_v = self._safe_field(
                            value, {**loc, "key": field},
                            COMPONENT_LINTER)
                        if safe_v is not None:
                            ent[field] = safe_v
                    desc = rule.get("description")
                    if desc is None:
                        pass
                    elif not isinstance(desc, str):
                        self.finding(
                            "formalism-malformed-source",
                            {**loc, "key": "description"},
                            component=COMPONENT_LINTER)
                    else:
                        safe_desc = self.safe_text(desc, loc,
                                                   COMPONENT_LINTER)
                        if safe_desc is not None:
                            ent["description"] = safe_desc
                    locators = [dict(loc)]
                    if safe_rid in ("H-2", "H-5"):
                        locators.append(
                            {"root": "plans", "path": HARDENING_MD,
                             "heading": HEADING_2_2A})
                    ent["locators"] = locators
                    self.entity(ent)
        self.rules_ok = True

    def _boundary_map(self, doc: Mapping[str, Any]) -> None:
        bmap = doc.get("boundary_slug_map")
        if bmap is None:
            return
        if not isinstance(bmap, dict):
            self.finding(
                "formalism-malformed-source",
                {"root": "plans", "path": RULES_JSON,
                 "key": "boundary_slug_map"},
                component=COMPONENT_LINTER)
            return
        symbolic: dict[str, str] = {}
        cont_loc = {"root": "plans", "path": RULES_JSON,
                    "key": "boundary_slug_map"}
        for key, value in bmap.items():
            if key in ("source_note", "source_files", "classes"):
                continue
            # The key is an output key channel: safe-texted (absolute
            # tokens rewritten in place, sensitive content a hard finding
            # with the entry dropped). The raw key never reaches the
            # finding locator.
            safe_key = self.safe_text(key, cont_loc, COMPONENT_LINTER)
            if safe_key is None:
                continue
            loc = {"root": "plans", "path": RULES_JSON,
                   "key": f"boundary_slug_map.{safe_key}"}
            if not isinstance(value, str):
                self.finding("formalism-malformed-source", loc,
                             component=COMPONENT_LINTER)
                continue
            safe = self.safe_text(value, loc, COMPONENT_LINTER)
            if safe is not None:
                symbolic[safe_key] = safe
        files: dict[str, dict] = {}
        raw_files = bmap.get("source_files")
        files_loc = {"root": "plans", "path": RULES_JSON,
                     "key": "boundary_slug_map.source_files"}
        if raw_files is not None:
            if not isinstance(raw_files, dict):
                self.finding("formalism-malformed-source", files_loc,
                             component=COMPONENT_LINTER)
            else:
                for name, rel in raw_files.items():
                    # The name is an output key channel (same treatment as
                    # the symbolic keys); the raw name never reaches the
                    # finding locator.
                    safe_name = self.safe_text(name, files_loc,
                                               COMPONENT_LINTER)
                    if safe_name is None:
                        continue
                    key = f"boundary_slug_map.source_files.{safe_name}"
                    key_loc = {"root": "plans", "path": RULES_JSON,
                               "key": key}
                    if not isinstance(rel, str):
                        self.finding("formalism-malformed-source", key_loc,
                                     component=COMPONENT_LINTER)
                        continue
                    channel = self.locator_channel(rel, key_loc,
                                                   COMPONENT_LINTER)
                    if channel is None:
                        continue
                    root_id = (channel.get("root")
                               or self.resolve_root_for(rel))
                    if root_id is None:
                        self.finding(
                            "formalism-missing-fact", key_loc,
                            component=COMPONENT_LINTER,
                            level="warning", impact="soft")
                        continue
                    files[safe_name] = {
                        "root": root_id, "path": channel["path"]}
        out: dict[str, Any] = {}
        if symbolic:
            out["symbolic_values"] = symbolic
        if files:
            out["source_files"] = files
        if out:
            self.boundary_map = out
        classes = bmap.get("classes")
        classes_loc = {"root": "plans", "path": RULES_JSON,
                       "key": "boundary_slug_map.classes"}
        if classes is not None:
            if not isinstance(classes, list):
                self.finding("formalism-malformed-source", classes_loc,
                             component=COMPONENT_LINTER)
            else:
                for j, name in enumerate(classes):
                    # The class name mints the ``boundary:{name}`` entity
                    # id; a rejected value mints nothing.
                    safe_name = self._safe_id(
                        name, {**classes_loc, "index": j},
                        COMPONENT_LINTER)
                    if safe_name is None:
                        continue
                    self.entity({
                        "id": f"boundary:{safe_name}",
                        "kind": "boundary-class",
                        "name": safe_name,
                        "source": dict(classes_loc),
                    })

    # -- expected_verdicts_ev12.json -------------------------------------------

    def _evidence_locator(self, rel: str, key: str) -> dict | None:
        """One adjudication evidence file, gated through the locator
        channel: an absolute or ``..`` value is a hard
        ``formalism-path-alias`` finding and is never concatenated raw
        into the locator path."""
        loc = {"root": "plans", "path": EXPECTED_VERDICTS_JSON, "key": key}
        channel = self.locator_channel(rel, loc, COMPONENT_LINTER)
        if channel is None:
            return None
        if "root" in channel:
            # Beneath-root absolute token: the channel path is already
            # root-relative; no TOOLS_DIR prefix.
            path = channel["path"]
            root_id = channel["root"]
        else:
            path = f"{TOOLS_DIR}/{channel['path']}"
            root_id = self.resolve_root_for(path) or "plans"
        item: dict[str, Any] = {
            "kind": "artifact-locator", "root": root_id, "path": path,
        }
        m = _REQ_FILE_RE.fullmatch(Path(path).name)
        if m:
            item["request_n"] = int(m.group(1))
        if root_id in self.roots and not (
                Path(self.roots[root_id]) / path).is_file():
            self.finding(
                "formalism-missing-fact",
                {"root": root_id, "path": path},
                level="warning", impact="soft")
        return item

    def parse_verdicts(self) -> dict[str, dict]:
        arms: dict[str, dict] = {}
        doc = self.read_json(
            "plans", EXPECTED_VERDICTS_JSON, component=COMPONENT_LINTER)
        if doc is _READ_FAILED:
            # read_json already recorded the hard finding.
            return arms
        if not isinstance(doc, dict):
            # A valid-JSON non-dict doc (list, string, JSON null) is a
            # hard malformed-source at the file locator, never a silent
            # return (round 5, m1).
            self.finding(
                "formalism-malformed-source",
                {"root": "plans", "path": EXPECTED_VERDICTS_JSON},
                component=COMPONENT_LINTER)
            return arms
        raw_arms = doc.get("arms")
        if raw_arms is not None:
            if not isinstance(raw_arms, list):
                self.finding(
                    "formalism-malformed-source",
                    {"root": "plans", "path": EXPECTED_VERDICTS_JSON,
                     "key": "arms"},
                    component=COMPONENT_LINTER)
            else:
                for i, arm in enumerate(raw_arms):
                    loc = {"root": "plans", "path": EXPECTED_VERDICTS_JSON,
                           "index": i}
                    if not isinstance(arm, dict):
                        self.finding("formalism-malformed-source", loc,
                                     component=COMPONENT_LINTER)
                        continue
                    # The fixture id mints ``request:{fid}:NNN`` entity
                    # ids and ``fixture:{fid}`` relationship targets; a
                    # value failing hygiene is dropped with a hard finding
                    # and mints nothing.
                    safe_fid = self._safe_id(arm.get("fixture"), loc,
                                             COMPONENT_LINTER)
                    if safe_fid is None:
                        continue
                    stamps: list[str] = []
                    stamps_raw = arm.get("ev_stamps")
                    if stamps_raw is None:
                        pass
                    elif not isinstance(stamps_raw, list):
                        # A non-list container (e.g. a string) is ONE hard
                        # finding, never iterated character-wise.
                        self.finding("formalism-malformed-source",
                                     {**loc, "key": "ev_stamps"},
                                     component=COMPONENT_LINTER)
                    else:
                        for j, s in enumerate(stamps_raw):
                            stamp_loc = {**loc, "key": "ev_stamps",
                                         "entry": j}
                            if not isinstance(s, str) or not s:
                                self.finding(
                                    "formalism-malformed-source",
                                    stamp_loc,
                                    component=COMPONENT_LINTER)
                                continue
                            safe_stamp = self.safe_text(s, stamp_loc,
                                                        COMPONENT_LINTER)
                            if safe_stamp is not None:
                                stamps.append(safe_stamp)
                    info = {
                        "ev_stamps": stamps,
                        "expected_exit_code": self._int_field(
                            arm.get("expected_exit_code"),
                            {**loc, "key": "expected_exit_code"},
                            COMPONENT_LINTER),
                        "expected_drift_findings": self._int_field(
                            arm.get("expected_drift_findings"),
                            {**loc,
                             "key": "expected_drift_findings"},
                            COMPONENT_LINTER),
                        "source_dir": self._source_dir(
                            arm.get("source_dir"), loc),
                    }
                    reqs_raw = arm.get("requests")
                    if reqs_raw is None:
                        pass
                    elif not isinstance(reqs_raw, list):
                        self.finding("formalism-malformed-source",
                                     {**loc, "key": "requests"},
                                     component=COMPONENT_LINTER)
                    else:
                        for k, req in enumerate(reqs_raw):
                            req_loc = {**loc, "key": "requests",
                                       "entry": k}
                            if not isinstance(req, dict):
                                self.finding(
                                    "formalism-malformed-source", req_loc,
                                    component=COMPONENT_LINTER)
                                continue
                            n = req.get("n")
                            if not isinstance(n, int) or isinstance(n, bool):
                                self.finding(
                                    "formalism-malformed-source",
                                    {**req_loc, "key": "n"},
                                    component=COMPONENT_LINTER)
                                continue
                            if n < 0:
                                # A negative request number would mint an
                                # ill-formed id (``request:...:-01``);
                                # hard malformed, no id minted (round 5,
                                # m7).
                                self.finding(
                                    "formalism-malformed-source",
                                    {**req_loc, "key": "n"},
                                    component=COMPONENT_LINTER)
                                continue
                            req_loc = {**loc, "request_n": n}
                            req_id = f"request:{safe_fid}:{n:03d}"
                            ent = {
                                "id": req_id,
                                "kind": "request-expectation",
                                "fixture_id": safe_fid,
                                "n": n,
                                "source": req_loc,
                            }
                            for field in ("method", "model", "row_class",
                                          "verdict"):
                                value = req.get(field)
                                if value is None:
                                    ent[field] = None
                                else:
                                    safe_v = self._safe_field(
                                        value, {**req_loc, "key": field},
                                        COMPONENT_LINTER)
                                    if safe_v is not None:
                                        ent[field] = safe_v
                            # path is a wire route (data, not a path):
                            # scan-only per the coordinator adjudication —
                            # no absolute-token rewrite.
                            path_val = req.get("path")
                            if path_val is None:
                                ent["path"] = None
                            else:
                                safe_p = self._scan_only(
                                    path_val, {**req_loc, "key": "path"},
                                    COMPONENT_LINTER)
                                if safe_p is not None:
                                    ent["path"] = safe_p
                            ent["hard"] = self._finding_list(
                                req.get("hard"), {**req_loc, "key": "hard"},
                                COMPONENT_LINTER)
                            ent["soft"] = self._finding_list(
                                req.get("soft"), {**req_loc, "key": "soft"},
                                COMPONENT_LINTER)
                            self.entity(ent)
                            self.rel("belongs-to", req_id,
                                     f"fixture:{safe_fid}")
                    if safe_fid in arms:
                        # A repeated fixture id would silently overwrite
                        # the first arm's verdict fields. Stated choice:
                        # keep-first (mirrors the entity id-collision
                        # rule); the duplicate's fields are dropped with
                        # a hard finding at the second arm's locator
                        # (round 5, m6).
                        self.finding(
                            "formalism-malformed-source",
                            {**loc, "key": "fixture"},
                            component=COMPONENT_LINTER)
                    else:
                        arms[safe_fid] = info
        raw_adjs = doc.get("adjudications")
        if raw_adjs is not None:
            if not isinstance(raw_adjs, list):
                self.finding(
                    "formalism-malformed-source",
                    {"root": "plans", "path": EXPECTED_VERDICTS_JSON,
                     "key": "adjudications"},
                    component=COMPONENT_LINTER)
            else:
                for i, adj in enumerate(raw_adjs):
                    loc = {"root": "plans", "path": EXPECTED_VERDICTS_JSON,
                           "index": i}
                    if not isinstance(adj, dict):
                        self.finding("formalism-malformed-source", loc,
                                     component=COMPONENT_LINTER)
                        continue
                    # The adjudication id mints the entity id; a rejected
                    # value mints nothing and drops the record.
                    safe_aid = self._safe_id(adj.get("id"), loc,
                                             COMPONENT_LINTER)
                    if safe_aid is None:
                        continue
                    evidence: list[dict] = []
                    raw_evidence = adj.get("evidence_files")
                    if raw_evidence is None:
                        raw_evidence = []
                    if not isinstance(raw_evidence, list):
                        self.finding(
                            "formalism-malformed-source",
                            {"root": "plans", "path": EXPECTED_VERDICTS_JSON,
                             "index": i,
                             "key": f"adjudications[{i}].evidence_files"},
                            component=COMPONENT_LINTER)
                    else:
                        for j, f in enumerate(raw_evidence):
                            if not isinstance(f, str) or not f:
                                # Malformed entries are findings, never
                                # silent skips.
                                self.finding(
                                    "formalism-malformed-source",
                                    {"root": "plans",
                                     "path": EXPECTED_VERDICTS_JSON,
                                     "index": i,
                                     "key":
                                         f"adjudications[{i}]"
                                         f".evidence_files[{j}]",
                                     "entry_type":
                                         type(f).__name__
                                         if not isinstance(f, str)
                                         else "empty"},
                                    component=COMPONENT_LINTER)
                                continue
                            item = self._evidence_locator(
                                f,
                                f"adjudications[{i}].evidence_files[{j}]")
                            if item is not None:
                                evidence.append(item)
                    ent = {
                        "id": f"adjudication:{safe_aid}",
                        "kind": "adjudication",
                        "adj_id": safe_aid,
                        "evidence": evidence,
                        "source": loc,
                    }
                    for src, dst in (("arm", "fixture_id"),
                                     ("rule", "rule"),
                                     ("class", "class")):
                        value = adj.get(src)
                        if value is None:
                            ent[dst] = None
                        else:
                            safe_v = self._safe_field(
                                value, {**loc, "key": src},
                                COMPONENT_LINTER)
                            if safe_v is not None:
                                ent[dst] = safe_v
                    for src in ("n", "hits"):
                        value = adj.get(src)
                        if value is None:
                            ent[src] = None
                        else:
                            iv = self._int_field(
                                value, {**loc, "key": src},
                                COMPONENT_LINTER)
                            if iv is not None and iv < 0:
                                # Negative request numbers / hit counts
                                # are malformed, never recorded (round 5,
                                # m7).
                                self.finding(
                                    "formalism-malformed-source",
                                    {**loc, "key": src},
                                    component=COMPONENT_LINTER)
                                iv = None
                            if iv is not None:
                                ent[src] = iv
                    text = adj.get("adjudication")
                    if text is None:
                        pass
                    elif not isinstance(text, str):
                        self.finding(
                            "formalism-malformed-source",
                            {**loc, "key": "adjudication"},
                            component=COMPONENT_LINTER)
                    else:
                        safe_text = self.safe_text(text, loc,
                                                   COMPONENT_LINTER)
                        if safe_text is not None:
                            ent["text"] = safe_text
                    self.entity(ent)
        return arms

    def _source_dir_unrooted(self, raw: str, loc: Mapping[str, Any]) -> None:
        """DESIGN.md §3: a source_dir that is not a named-root locator
        is represented by a finding plus hash, never the raw string."""
        self.finding(
            "formalism-source-dir-unrooted", dict(loc),
            component=COMPONENT_LINTER,
            level="warning", impact="soft",
            detail=[{"sha256": hashlib.sha256(raw.encode("utf-8")).hexdigest(),
                     "bytes": len(raw.encode("utf-8"))}])

    @staticmethod
    def _source_dir_clean(value: str) -> bool:
        """Clean-path gate for ``source_dir`` values (DESIGN.md §3).

        Aligned with ``paths._validate_posix_relative`` (round 5, m11):
        the same normative segment rules, with the live-corpus
        trailing-slash convention tolerated (one trailing ``/`` is
        stripped before validation). The former local
        ``_SOURCE_DIR_PATH_RE`` disagreed with the validator on
        trailing-dot values; the validator now decides in both
        directions.
        """
        if not value or value == "/":
            return False
        probe = value[:-1] if value.endswith("/") else value
        try:
            paths._validate_posix_relative(probe)
        except ContractError:
            return False
        return True

    def _resolve_source_dir_root(self, rel: str) -> str | None:
        """The first supplied root (sorted) holding ``rel`` as a file or
        directory.

        ``resolve_root_for`` is file-only (locator channels address
        files); ``source_dir`` names a capture directory, so a
        directory match is convertible as well (round 5, m9).
        """
        for root_id in sorted(self.roots):
            try:
                cand = Path(self.roots[root_id]) / rel
            except (OSError, TypeError, ValueError):
                continue
            if cand.is_file() or cand.is_dir():
                return root_id
        return None

    def _source_dir(self, value: Any,
                    loc: Mapping[str, Any]) -> Any:
        if value is None:
            return None
        if not isinstance(value, str):
            self.finding("formalism-malformed-source",
                         {**dict(loc), "key": "source_dir"},
                         component=COMPONENT_LINTER)
            return None
        tokens = _ABS_TOKEN_RE.findall(value)
        if not tokens:
            safe = self.safe_text(value, {**dict(loc), "key": "source_dir"},
                                  COMPONENT_LINTER)
            if safe is None:
                return None
            if self._source_dir_clean(safe):
                root_id = self._resolve_source_dir_root(safe)
                if root_id is None:
                    # Path-shaped but beneath no supplied named root:
                    # finding plus hash, raw value omitted (DESIGN.md §3).
                    self._source_dir_unrooted(value, dict(loc))
                    return None
                # Convertible: the value is represented as a rooted
                # locator, never the raw string (DESIGN.md §3). The
                # emitted path is the validated form: the one trailing
                # "/" that _source_dir_clean stripped before the
                # validator decided is not re-emitted (round 6, m-2).
                rel = safe[:-1] if safe.endswith("/") else safe
                return {"root": root_id, "path": rel}
            # Delta against DESIGN.md §3 ("raw `source_dir` strings are
            # never copied"), open for adjudication as Bead
            # apex-ayl.137.4: the free-text allowance is closed to the
            # descriptive annotation strings of the live corpus (they may
            # carry a slash inside the prose, e.g. "EV-13/EV-9-stamped",
            # but no traversal shape) and the value has already cleared
            # ``safe_text``. Any other non-clean value with
            # path-traversal shape or an absolute shape — a '..' run
            # (covers '../x' and 'a..b'), a '.' or '..' segment (covers
            # './x' and 'a/./b'), a '//' run, or a leading '/' — is a
            # finding plus hash, never copied raw. The leading '/' is
            # not redundant with _ABS_TOKEN_RE: that pattern requires
            # two or more segments, so '/etc' and '/' reach this branch
            # as whole-value absolutes (three-seat review, Seat C M-4).
            # A single-segment absolute inside prose remains covered by
            # the free-text allowance above; closing that residual is
            # part of the same open adjudication (Bead apex-ayl.137.4).
            if (
                ".." in safe
                or "//" in safe
                or safe.startswith("/")
                or any(seg in (".", "..") for seg in safe.split("/"))
            ):
                self._source_dir_unrooted(value, dict(loc))
                return None
            return safe
        out = []
        for token in tokens:
            norm = self._norm_token(token, loc, COMPONENT_LINTER)
            # The emitted normalized form (``@root/tail`` or
            # ``@external/base#sha``) passes the sensitive scan like
            # every other emitted path (locator_channel discipline): a
            # hit is a hard finding at the non-echoing arm locator and
            # the token is dropped, never emitted (round 6, MAJOR-A).
            hits = sensitive_matches(norm)
            if hits:
                self.finding(
                    "formalism-sensitive-output",
                    {**dict(loc), "key": "source_dir"},
                    component=COMPONENT_LINTER,
                    detail=[{"pattern": h["pattern"],
                             "offset": h["offset"]}
                            for h in hits])
                continue
            if norm not in out:
                out.append(norm)
        if any(t.startswith("@external/") for t in out):
            # Absolute tokens could not be mapped beneath a supplied
            # root: the hash form stays, and the rule requires the
            # finding as well.
            self._source_dir_unrooted(value, loc)
        return out or None

    # -- fixture arms (meta.json) ----------------------------------------------

    def _norm_provenance(self, value: Any, loc: Mapping[str, Any]) -> Any:
        if isinstance(value, dict):
            out: dict[str, Any] = {}
            cont_loc = {**loc, "key": "copied_from"}
            for j, (key, item) in enumerate(value.items()):
                if not isinstance(key, str) or not key:
                    self.finding("formalism-malformed-source",
                                 {**cont_loc, "entry": j},
                                 component=COMPONENT_CORPUS)
                    continue
                # The key is an output key channel: safe-texted, and the
                # raw key never reaches the finding locator.
                safe_key = self.safe_text(key, cont_loc, COMPONENT_CORPUS)
                if safe_key is None:
                    continue
                safe_item = self._norm_prov_item(
                    item, {**cont_loc, "key": f"copied_from.{safe_key}"})
                if safe_item is not None:
                    out[safe_key] = safe_item
            return out
        return self._norm_prov_item(value, {**loc, "key": "copied_from"})

    def _norm_prov_item(self, item: Any, loc: Mapping[str, Any]) -> Any:
        if not isinstance(item, str):
            # Hard finding with the key dropped (stated choice: never a
            # null value riding into the provenance).
            self.finding("formalism-malformed-source", dict(loc),
                         component=COMPONENT_CORPUS)
            return None
        if not _ABS_TOKEN_RE.search(item):
            # Source-authored relative reference text: safe reference,
            # retained verbatim through the sensitive scan.
            return self.safe_text(item, dict(loc), COMPONENT_CORPUS)
        # Absolute tokens are rewritten IN PLACE: the surrounding
        # source-authored reference text is retained (coordinator
        # adjudication), then the result is sensitive-scanned.
        out = self.rewrite_absolute(item, dict(loc), COMPONENT_CORPUS)
        hits = sensitive_matches(out)
        if hits:
            self.finding(
                "formalism-sensitive-output", dict(loc),
                component=COMPONENT_CORPUS,
                detail=[{"pattern": h["pattern"], "offset": h["offset"]}
                        for h in hits])
            return None
        return out

    def _arm_oq_cites(self, arm_id: str, all_stamps: list[str],
                      loc: Mapping[str, Any]) -> None:
        open_qs = [
            m.group(0) for s in all_stamps if (m := _OQ_ID_RE.match(s))
        ]
        if not open_qs:
            return
        if self.oq_section_id is None:
            self.finding("formalism-missing-fact", loc)
            return
        locator = {
            "root": "plans", "path": HARDENING_MD,
            "heading": self.oq_section_heading,
        }
        for oid in open_qs:
            if oid not in self.oq_registry:
                self.finding("formalism-unknown-citation", loc)
                continue
            self.rel("cites", f"fixture:{arm_id}", self.oq_section_id,
                     locator=locator)

    def parse_fixture_arms(self, verdict_arms: Mapping[str, dict]) -> None:
        base = Path(self.roots["plans"]) / FIXTURES_DIR \
            if "plans" in self.roots else None
        dirs: list[Path] = []
        if base is not None and base.is_dir():
            dirs = sorted(
                (d for d in base.iterdir() if d.is_dir() and not d.is_symlink()),
                key=lambda d: d.name,
            )
        seen: set[str] = set()
        for d in dirs:
            arm_id = d.name
            loc = {"root": "plans",
                   "path": f"{FIXTURES_DIR}/{arm_id}/meta.json"}
            # The directory name mints the ``fixture:{arm_id}`` entity
            # id; a name failing hygiene is dropped with a hard finding
            # and mints nothing.
            if self._safe_id(arm_id, loc, COMPONENT_CORPUS) is None:
                continue
            seen.add(arm_id)
            meta = self.read_json(
                "plans", f"{FIXTURES_DIR}/{arm_id}/meta.json",
                component=COMPONENT_CORPUS)
            if meta is _READ_FAILED:
                # Missing/undecodable file: read_json already recorded
                # the hard malformed-source finding.
                continue
            if not isinstance(meta, dict):
                # A successful parse of a non-dict doc (including JSON
                # null) is a hard malformed-source at its own path.
                self.finding("formalism-malformed-source", loc,
                             component=COMPONENT_CORPUS)
                continue
            ent: dict[str, Any] = {
                "id": f"fixture:{arm_id}",
                "kind": "fixture-arm",
                "fixture_id": arm_id,
                "source": loc,
            }
            kind = meta.get("kind")
            if kind in NATIVE_KINDS:
                ent["native_kind"] = kind
            elif isinstance(kind, str):
                self.finding("formalism-kind-enum-violation", loc)
            elif kind is not None:
                # A present non-string kind is a hard malformed-source at
                # the exact key, never a silent skip (round 5, m4).
                self.finding("formalism-malformed-source",
                             {**loc, "key": "kind"},
                             component=COMPONENT_CORPUS)
            copied_at = meta.get("copied_at")
            if isinstance(copied_at, str):
                safe_at = self.safe_text(
                    copied_at, {**loc, "key": "copied_at"},
                    COMPONENT_CORPUS)
                if safe_at is not None:
                    ent["copied_at"] = safe_at
            elif copied_at is not None:
                self.finding("formalism-malformed-source",
                             {**loc, "key": "copied_at"},
                             component=COMPONENT_CORPUS)
            stamps_raw = meta.get("ev_stamps")
            stamps_map: dict[str, list[str]] = {}
            if stamps_raw is None:
                pass
            elif not isinstance(stamps_raw, dict):
                # A non-dict container is ONE hard finding, never
                # iterated.
                self.finding("formalism-malformed-source",
                             {**loc, "key": "ev_stamps"},
                             component=COMPONENT_CORPUS)
            else:
                for j, (pathspec, stamps) in enumerate(stamps_raw.items()):
                    entry_loc = {**loc, "key": "ev_stamps", "entry": j}
                    if not isinstance(pathspec, str) or not pathspec:
                        self.finding("formalism-malformed-source",
                                     entry_loc,
                                     component=COMPONENT_CORPUS)
                        continue
                    # Pathspec keys get the same treatment as values:
                    # absolute tokens externalized, sensitive content a
                    # hard finding with the entry dropped.
                    safe_key = self.safe_text(pathspec, entry_loc,
                                              COMPONENT_CORPUS)
                    if safe_key is None:
                        continue
                    if not isinstance(stamps, list):
                        self.finding("formalism-malformed-source",
                                     entry_loc,
                                     component=COMPONENT_CORPUS)
                        continue
                    kept = []
                    for k, s in enumerate(stamps):
                        if not isinstance(s, str):
                            self.finding(
                                "formalism-malformed-source",
                                {**entry_loc, "stamp": k},
                                component=COMPONENT_CORPUS)
                            continue
                        safe_s = self.safe_text(
                            s, {**entry_loc, "stamp": k},
                            COMPONENT_CORPUS)
                        if safe_s is not None:
                            kept.append(safe_s)
                    if kept:
                        stamps_map[safe_key] = kept
            if stamps_map:
                ent["ev_stamps"] = stamps_map
            copied_from = meta.get("copied_from")
            if copied_from is not None:
                prov = self._norm_provenance(copied_from, loc)
                if prov:
                    ent["provenance"] = {"text": prov}
            sup = meta.get("supersession_note")
            if isinstance(sup, str):
                safe_sup = self.safe_text(
                    sup, {**loc, "key": "supersession_note"},
                    COMPONENT_CORPUS)
                if safe_sup is not None:
                    ent["supersession"] = safe_sup
            elif sup is not None:
                self.finding("formalism-malformed-source",
                             {**loc, "key": "supersession_note"},
                             component=COMPONENT_CORPUS)
            vinfo = verdict_arms.get(arm_id)
            if vinfo is None:
                self.finding(
                    "formalism-missing-fact", loc,
                    level="warning", impact="soft")
            else:
                if vinfo["ev_stamps"]:
                    ent["expected_ev_stamps"] = list(vinfo["ev_stamps"])
                if vinfo["source_dir"] is not None:
                    ent["source_dir"] = vinfo["source_dir"]
                for field in ("expected_exit_code", "expected_drift_findings"):
                    if vinfo[field] is not None:
                        ent[field] = vinfo[field]
                all_stamps = (
                    [s for v in stamps_map.values() for s in v]
                    + list(vinfo["ev_stamps"])
                )
                self._arm_oq_cites(arm_id, all_stamps, loc)
            self.entity(ent)
        for fid in verdict_arms:
            if fid not in seen:
                self.finding(
                    "formalism-missing-fact",
                    {"root": "plans", "path": EXPECTED_VERDICTS_JSON,
                     "key": fid},
                    level="warning", impact="soft")

    # -- artifacts and the generated-rule coupling gate ------------------------

    def _register_artifact(
        self,
        root_id: str,
        rel: str,
        role: str | None = None,
        generation_state: str = "source-authored",
        component: str = COMPONENT_LINTER,
    ) -> None:
        if root_id not in self.roots:
            return
        # Walked filenames are disk-derived: a raw-key/canary filename is
        # a hard sensitive finding and the artifact is dropped, never
        # registered (coordinator decision, round 5, m8). The locator
        # names the parent directory only — the raw name rides along as a
        # hash, never echoed.
        hits = sensitive_matches(rel)
        if hits:
            # A top-level (no-slash) file has no parent directory: the
            # root dir is the non-empty non-echoing locator, never an
            # empty path (round 9, M-5).
            parent = rel.rsplit("/", 1)[0] if "/" in rel else "."
            self.finding(
                "formalism-sensitive-output",
                {"root": root_id, "path": parent},
                component=component,
                detail=[{"pattern": h["pattern"], "offset": h["offset"]}
                        for h in hits],
                rel_sha256=hashlib.sha256(rel.encode("utf-8")).hexdigest())
            return
        aid = f"artifact:{root_id}:{rel}"
        existing = self.entities.get(aid)
        if existing is not None:
            # Dedup must not swallow an explicit registration: an
            # explicit role (or non-default generation state) upgrades
            # the earlier default instead of being dropped.
            if role is not None and existing.get("role") != role:
                existing["role"] = role
            if (generation_state != "source-authored"
                    and existing.get("generation_state") != generation_state):
                existing["generation_state"] = generation_state
            return
        if root_id == "worktree" and rel in (RULES_GENERATED_RS, HARD_RULES_JSON):
            generation_state = "declared-generated"
            role = role or "generated-target"
        if role is None:
            if root_id == "worktree" and rel.endswith(".rs"):
                role = "test-source"
            elif root_id == "worktree":
                role = "test-fixture"
            elif root_id == "plans":
                role = "evidence-report"
            else:
                role = "source"
        ent: dict[str, Any] = {
            "id": aid,
            "kind": "artifact",
            "root": root_id,
            "path": rel,
            "role": role,
            "generation_state": generation_state,
            "source": {"root": root_id, "path": rel},
        }
        if rel == RULES_JSON and self.boundary_map is not None:
            ent["boundary_map"] = self.boundary_map
        if rel == PROJECTION_TESTS_RS:
            symbols = self._verify_h7_symbols()
            if symbols:
                ent["symbols"] = symbols
            if "rule:H-7" in self.entities:
                for sym in symbols:
                    self.rel(
                        "enforced-by", "rule:H-7", aid,
                        locator={"root": "worktree",
                                 "path": PROJECTION_TESTS_RS,
                                 "symbol": sym})
        self.entity(ent)

    def _verify_h7_symbols(self) -> list[str]:
        loc = {"root": "worktree", "path": PROJECTION_TESTS_RS}
        data = self.read("worktree", PROJECTION_TESTS_RS)
        if data is None:
            self.finding(
                "formalism-missing-fact", loc, component=COMPONENT_LINTER)
            return []
        text = data.decode("utf-8", "replace")
        found = []
        for sym in H7_RUST_SYMBOLS:
            if re.search(rf"\bfn {re.escape(sym)}\s*\(", text):
                found.append(sym)
            else:
                self.finding(
                    "formalism-missing-fact",
                    {"root": "worktree", "path": PROJECTION_TESTS_RS,
                     "symbol": sym},
                    component=COMPONENT_LINTER)
        return found

    def parse_artifacts(self) -> None:
        # Container discipline (round 10, MAJOR-2): None is a clean
        # empty walk (today's behavior); a non-list container is ONE
        # hard finding at the catalog-file locator and the walk is
        # skipped (no soft missing-fact) — an int escapes as a raw
        # TypeError and a string char-iterates into a silent input-walk
        # skip; a non-dict row is ONE hard finding at the row index and
        # only that row is skipped — a valid linter row after a bad row
        # must still be found. Every row is validated BEFORE the linter
        # search: the round-10 break-on-match left rows after the
        # linter row unchecked — a malformed trailing row vanished
        # silently (round 11, M-1).
        components = self.catalog.get("components")
        if components is None:
            components = []
        elif not isinstance(components, list):
            self.finding(
                "formalism-malformed-source",
                {"root": "worktree",
                 "path": "smoke/eval_harness/catalog.json",
                 "key": "components"},
                component=COMPONENT_LINTER)
            return
        # Validate every row BEFORE the linter search: the round-10
        # break-on-match left rows after the linter row unchecked — a
        # malformed trailing row vanished silently (round 11, M-1).
        for j, row in enumerate(components):
            if not isinstance(row, dict):
                self.finding(
                    "formalism-malformed-source",
                    {"root": "worktree",
                     "path": "smoke/eval_harness/catalog.json",
                     "key": "components", "index": j},
                    component=COMPONENT_LINTER)
        comp = None
        comp_index = None
        for j, row in enumerate(components):
            if isinstance(row, dict) and row.get("id") == COMPONENT_LINTER:
                if comp is None:
                    # First-match-wins for collection (unchanged).
                    comp = row
                    comp_index = j
                else:
                    # A second well-formed linter-id row was silently
                    # ignored pre-fix (its inputs never walked, zero
                    # findings) — the K-1 "every row accounted for"
                    # principle, with no mirror of the load-time
                    # duplicate-id rejection: one hard at the row
                    # index, row contents never echoed (round 15,
                    # QC M-2).
                    self.finding(
                        "formalism-malformed-source",
                        {"root": "worktree",
                         "path": "smoke/eval_harness/catalog.json",
                         "key": "components", "index": j},
                        component=COMPONENT_LINTER)
        comp_root = comp.get("root", "plans") if isinstance(comp, dict) else "plans"
        if comp is None:
            self.finding(
                "formalism-missing-fact",
                {"root": "plans", "path": RULES_JSON},
                component=COMPONENT_LINTER,
                level="warning", impact="soft")
        if comp is not None:
            catalog_loc = {"root": "worktree",
                           "path": "smoke/eval_harness/catalog.json"}
            # A linter row whose own root the context does not supply is
            # ONE hard finding at the row's catalog locator and every
            # bare (no-colon) input is then skipped: an unknown root
            # would otherwise KeyError in the ``/**`` glob branch,
            # TypeError in the unhashable-root ``in`` guard, or silently
            # drop a non-glob declared artifact (DESIGN.md §3; round 8,
            # MINOR-1 one-hard-and-drop; round 12, F-1). Rooted items
            # validate their own spec_root and are unaffected; the bad
            # root value is never echoed.
            bare_root_ok = (isinstance(comp_root, str)
                            and comp_root in self.roots)
            if not bare_root_ok:
                self.finding(
                    "formalism-malformed-source",
                    {**catalog_loc, "key": "components",
                     "index": comp_index},
                    component=COMPONENT_LINTER)
            raw_inputs = comp.get("inputs")
            if raw_inputs is None:
                raw_inputs = []
            elif not isinstance(raw_inputs, list):
                # A truthy non-list inputs container is never iterated:
                # a string char-iterates (every character would mint a
                # fabricated artifact while the declared artifacts
                # vanish) and an int escapes as a raw TypeError. ONE
                # hard finding at the catalog-file locator (no index
                # for a container violation) and the input walk is
                # skipped (DESIGN.md §8; round 7, M-1).
                self.finding(
                    "formalism-malformed-source",
                    {**catalog_loc, "key": f"{COMPONENT_LINTER}.inputs"},
                    component=COMPONENT_LINTER)
                raw_inputs = []
            for j, item in enumerate(raw_inputs):
                if not isinstance(item, str):
                    # A non-string catalog input row is a hard finding
                    # naming the row (component id + item index) in the
                    # catalog file, never a silent skip (round 6, m-3).
                    self.finding(
                        "formalism-malformed-source",
                        {**catalog_loc,
                         "key": f"{COMPONENT_LINTER}.inputs",
                         "index": j},
                        component=COMPONENT_LINTER)
                    continue
                item_loc = {**catalog_loc,
                            "key": f"{COMPONENT_LINTER}.inputs",
                            "index": j}
                # A NUL passes every shape gate and then either rides
                # verbatim into a minted artifact id (bare/rooted) or
                # silently drops (glob): one hard at the item locator
                # covers all three variants, raw value never echoed
                # (round 15, QC M-1; mirrors the inventory
                # _reject_nul-at-every-channel discipline).
                if "\x00" in item:
                    self.finding("formalism-malformed-source", item_loc,
                                 component=COMPONENT_LINTER)
                    continue
                if ":" in item:
                    spec_root, spec_rel = item.split(":", 1)
                    if spec_root not in self.roots:
                        # A rooted spec naming a root the context does
                        # not supply is ONE hard finding and the item
                        # is dropped — it never falls through to the
                        # bare branch (the whole raw value, colon and
                        # all, would be minted into the artifact id)
                        # (DESIGN.md §8; round 8, MINOR-1).
                        self.finding(
                            "formalism-malformed-source", item_loc,
                            component=COMPONENT_LINTER)
                        continue
                    if _catalog_spec_shape_violation(spec_rel):
                        self.finding(
                            "formalism-malformed-source", item_loc,
                            component=COMPONENT_LINTER)
                        continue
                    if _catalog_spec_escapes(spec_rel):
                        self.finding(
                            "formalism-malformed-source", item_loc,
                            component=COMPONENT_LINTER)
                        continue
                    self._collect_artifacts(spec_root, spec_rel)
                    continue
                if not bare_root_ok:
                    # The one row-level hard covers the whole row: bare
                    # items never reach _collect_artifacts (round 12,
                    # F-1).
                    continue
                if _catalog_spec_shape_violation(item):
                    # A bare spec with an empty/whitespace-only body,
                    # a leading ``.`` segment, or an empty segment
                    # never mints an artifact id (DESIGN.md §8; round
                    # 8, MINOR-1).
                    self.finding(
                        "formalism-malformed-source", item_loc,
                        component=COMPONENT_LINTER)
                    continue
                if _catalog_spec_escapes(item):
                    self.finding(
                        "formalism-malformed-source", item_loc,
                        component=COMPONENT_LINTER)
                    continue
                self._collect_artifacts(comp_root, item)
        # Design-pinned core artifacts (dedup-safe against the catalog walk).
        self._register_artifact("plans", RULES_JSON, role="rule-source")
        self._register_artifact("plans", GENERATOR_PY, role="tool-source")
        self._register_artifact("plans", LINTER_PY, role="tool-source")
        self._register_artifact("worktree", RULES_GENERATED_RS)
        self._register_artifact("worktree", PROJECTION_TESTS_RS)
        self._register_artifact("worktree", HARD_RULES_JSON)
        for gen in (RULES_GENERATED_RS, HARD_RULES_JSON):
            gid = f"artifact:worktree:{gen}"
            if gid in self.entities:
                self.rel("generated-from", gid, f"artifact:plans:{RULES_JSON}")
                self.rel("generated-from", gid, f"artifact:plans:{GENERATOR_PY}")

    def _collect_artifacts(self, root_id: str, rel: str) -> None:
        if rel.endswith("/**"):
            base = Path(self.roots[root_id]) / rel[:-3]
            if not base.is_dir():
                return
            for dirpath, dirnames, filenames in os.walk(base, followlinks=False):
                dirnames.sort()
                for name in sorted(filenames):
                    full = Path(dirpath) / name
                    relp = full.relative_to(self.roots[root_id]).as_posix()
                    self._register_artifact(root_id, relp)
        else:
            self._register_artifact(root_id, rel)

    def run_drift_gate(self) -> None:
        """Byte-compare the pure ``derive()`` seam against the committed
        generated targets; mismatch is a hard generated-rule-drift finding
        rooted at the target. Never invokes the generator CLI or the
        linter, and writes nothing."""
        if self.read("plans", GENERATOR_PY) is None:
            self.finding(
                "formalism-malformed-source",
                {"root": "plans", "path": GENERATOR_PY},
                component=COMPONENT_LINTER)
            return
        if not self.rules_ok:
            # A malformed rules source is already recorded; the seam has no
            # valid input to derive from.
            return
        if "plans" not in self.roots or "worktree" not in self.roots:
            return
        try:
            module = _load_generator_seam(
                Path(self.roots["plans"]) / GENERATOR_PY)
            fixture_bytes, table_bytes = module.derive(
                Path(self.roots["plans"]) / RULES_JSON)
        except Exception:
            self.finding(
                "formalism-malformed-source",
                {"root": "plans", "path": RULES_JSON},
                component=COMPONENT_LINTER)
            return
        for target_rel, derived in (
            (HARD_RULES_JSON, fixture_bytes),
            (RULES_GENERATED_RS, table_bytes),
        ):
            committed = self.read("worktree", target_rel)
            if committed is None or committed != derived:
                self.finding(
                    "formalism-generated-rule-drift",
                    {"root": "worktree", "path": target_rel},
                    component=COMPONENT_LINTER,
                    level="error",
                    impact="hard",
                    derived_sha256=hashlib.sha256(derived).hexdigest(),
                    committed_sha256=(
                        hashlib.sha256(committed).hexdigest()
                        if committed is not None else None
                    ),
                )


def discover(
    ctx: AdapterContext, requested_runs: Iterable = ()
) -> AdapterResult:
    """Discover the formalism corpus entities (DESIGN.md §4).

    Read-only: the plans corpus and the worktree are never written. The
    corpus adapter emits no runs (``requested_runs`` is accepted for
    protocol uniformity and ignored); historical run evidence belongs to
    the run adapters.
    """
    del requested_runs  # corpus adapter: runs are never folded in
    st = _State(ctx)
    st.parse_curated_sections()
    st.parse_rules()
    verdict_arms = st.parse_verdicts()
    st.parse_fixture_arms(verdict_arms)
    st.parse_artifacts()
    st.run_drift_gate()
    entities = tuple(sorted(st.entities.values(), key=lambda e: e["id"]))
    relationships = tuple(
        sorted(
            st._rel_records,
            key=lambda r: (
                r["kind"],
                r["source"],
                r["target"],
                _locator_key_tolerant(r.get("locator")),
            ),
        )
    )
    findings = tuple(
        sorted(
            st.findings,
            key=lambda f: (
                f["component"],
                f["code"],
                _locator_key_tolerant(f.get("source")),
                f["occurrence"],
            ),
        )
    )
    return AdapterResult(
        entities=entities,
        relationships=relationships,
        runs=(),
        findings=findings,
    )
