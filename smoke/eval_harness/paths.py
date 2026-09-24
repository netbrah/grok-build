"""Portable path primitives for the evaluation harness.

Rooted paths are ``ROOT:POSIX_RELATIVE_PATH`` strings (DESIGN.md §2). All
checks are lexical: containment never follows inventory symlinks, and
absolute source-text tokens are normalized against named roots without
resolving, opening, or following anything (DESIGN.md §3 stable record
rules). Import only through the canonical package name
``smoke.eval_harness.paths``.
"""
from __future__ import annotations

import hashlib
import os
import re
import stat
from dataclasses import dataclass
from pathlib import Path
from typing import Mapping

from .contract import ContractError

__all__ = ["RootedPath", "normalize_path_token", "resolve_rooted"]

_ROOT_SLUG_RE = re.compile(r"^[a-z][a-z0-9-]*$")
_SEGMENT_RE = re.compile(r"^[A-Za-z0-9._*%-]+$")


def _validate_posix_relative(path: str) -> None:
    """Raise ContractError unless ``path`` is a clean relative POSIX path.

    Forward slashes only, non-empty segments, no ``.``/``..`` segments, no
    backslashes. Purely lexical.
    """
    if not isinstance(path, str) or not path:
        raise ContractError("path must be a non-empty string", code="path-shape")
    if path.startswith("/"):
        raise ContractError(f"path {path!r} is absolute", code="path-absolute")
    if "\\" in path:
        raise ContractError(f"path {path!r} contains a backslash", code="path-backslash")
    for segment in path.split("/"):
        if not _SEGMENT_RE.fullmatch(segment):
            raise ContractError(
                f"path {path!r} has invalid segment {segment!r}", code="path-shape"
            )
        if segment in (".", ".."):
            raise ContractError(
                f"path {path!r} has a {segment!r} segment", code="path-dot-segment"
            )


def _validate_root_slug(root: str) -> None:
    if not isinstance(root, str) or not _ROOT_SLUG_RE.fullmatch(root):
        raise ContractError(
            f"root {root!r} is not a lowercase slug", code="root-shape"
        )


@dataclass(frozen=True)
class RootedPath:
    """A named root plus a normalized POSIX-relative path.

    Constructed from a ``(root, path)`` pair or parsed from the single
    accepted string form ``ROOT:POSIX_RELATIVE_PATH``.
    """

    root: str
    path: str

    def __post_init__(self) -> None:
        _validate_root_slug(self.root)
        _validate_posix_relative(self.path)

    @classmethod
    def parse(cls, token: str) -> "RootedPath":
        """Parse ``ROOT:POSIX_RELATIVE_PATH``; nothing else is accepted."""
        if not isinstance(token, str):
            raise ContractError("rooted token must be a string", code="path-shape")
        root, sep, path = token.partition(":")
        if not sep:
            raise ContractError(
                f"rooted token {token!r} is not of the form ROOT:POSIX_RELATIVE_PATH",
                code="path-shape",
            )
        if not path:
            raise ContractError(
                f"rooted token {token!r} has an empty path part", code="path-shape"
            )
        if ":" in path:
            raise ContractError(
                f"rooted token {token!r} has a colon inside its path part",
                code="path-shape",
            )
        return cls(root, path)


def resolve_rooted(
    roots: Mapping[str, object],
    rooted: RootedPath,
    *,
    require_regular: bool = False,
) -> "os.PathLike":
    """Resolve a rooted path beneath its named root.

    Containment is enforced lexically: the relative part is validated at
    construction time and the result is the plain join of the supplied root
    and that part — no ``resolve()``, no symlink following. With
    ``require_regular=True`` the final entry must be a regular file per
    ``lstat`` (a symlink, even a dangling one, is not regular and its
    target is never opened). A root directory of ``.`` (any spelling:
    ``"."``, ``"./"``, ``Path(".")``, ``""``) names the current directory;
    every validated relative part is contained under it by construction.
    """
    if rooted.root not in roots:
        raise ContractError(
            f"root {rooted.root!r} is not among the supplied roots", code="unknown-root"
        )
    root_dir = os.fspath(roots[rooted.root])
    candidate = os.path.join(root_dir, rooted.path)
    # Lexical containment check (defense in depth; the path part already
    # cannot contain ``..`` or a leading slash). Under a "." root the
    # normalized candidate is a plain relative path and is contained.
    root_norm = os.path.normpath(root_dir)
    candidate_norm = os.path.normpath(candidate)
    if (
        root_norm != "."
        and candidate_norm != root_norm
        and not candidate_norm.startswith(root_norm + os.sep)
    ):
        raise ContractError(
            f"rooted path escapes root {rooted.root!r}", code="containment"
        )
    if require_regular:
        try:
            info = os.lstat(candidate)
        except OSError as exc:
            raise ContractError(
                f"regular file missing at {rooted.root}:{rooted.path} ({exc.strerror})",
                code="not-regular",
            ) from None
        if not stat.S_ISREG(info.st_mode):
            raise ContractError(
                f"{rooted.root}:{rooted.path} is not a regular file "
                "(symlinks are not followed)",
                code="not-regular",
            )
    return Path(candidate)


def normalize_path_token(token: str, roots: Mapping[str, object]) -> str:
    """Deterministically rewrite an absolute source-text path token.

    Relative tokens (symlink strings) are returned verbatim. An absolute
    token beneath a supplied named root becomes ``@<root>/<relative-path>``;
    when supplied roots nest, the longest matching lexical root prefix wins
    and an equal-length ambiguity is a hard error. Every other token becomes
    ``@external/<basename>#<full-SHA-256-of-the-original-token>``.
    Normalization is lexical: roots are never stat'ed, resolved, or opened.
    """
    if not isinstance(token, str) or not token:
        raise ContractError("token must be a non-empty string", code="path-shape")
    if not token.startswith("/"):
        return token
    token_norm = os.path.normpath(token)
    matches: list[tuple[str, str]] = []
    for root_id in sorted(roots):
        root_norm = os.path.normpath(os.fspath(roots[root_id]))
        if not root_norm or root_norm == "/":
            continue
        if token_norm.startswith(root_norm + "/"):
            matches.append((root_id, root_norm))
    if matches:
        longest = max(len(root_norm) for _, root_norm in matches)
        winners = [
            (root_id, root_norm)
            for root_id, root_norm in matches
            if len(root_norm) == longest
        ]
        if len(winners) > 1:
            ids = ", ".join(repr(root_id) for root_id, _ in winners)
            raise ContractError(
                f"equal-length root alias between {ids} for token {token!r}",
                code="root-alias",
            )
        root_id, root_norm = winners[0]
        return f"@{root_id}/{token_norm[len(root_norm) + 1:]}"
    base = token_norm.rsplit("/", 1)[-1]
    if not base:
        raise ContractError(f"token {token!r} has no basename", code="path-shape")
    digest = hashlib.sha256(token.encode("utf-8")).hexdigest()
    return f"@external/{base}#{digest}"
