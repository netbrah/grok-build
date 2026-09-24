"""The ``plan_corpus`` adapter (DESIGN.md §4).

Inventories every regular file and symlink under the three plans-corpus
trees (``parity-formalism/``, ``provenance/``, ``xwire/``) with
complete, race-safe coverage: untracked and ignored files are included,
symlinks are never followed, and a source change during the scan fails
the call with ``SourceChangedError``.

Metadata only: this adapter performs no curated-text extraction and
emits no entities — all heading/question range extraction and
``section:*`` entities belong exclusively to the formalism adapter
(Task 4). The inventory records themselves are produced by
``smoke.eval_harness.inventory.inventory_scope`` / ``inventory_all``
and merged into the evidence index at the writer boundary.
"""
from __future__ import annotations

from typing import Iterable

from smoke.eval_harness import inventory
from smoke.eval_harness.contract import (
    AdapterContext,
    AdapterResult,
    ContractError,
)

__all__ = ["SCOPE_ID", "discover"]

SCOPE_ID = "plans-corpus"


def discover(
    ctx: AdapterContext, requested_runs: Iterable = ()
) -> AdapterResult:
    """Inventory the three plans-corpus trees (metadata only).

    Rejects any nonempty ``requested_runs``: the plans corpus is a base
    scope and historical run evidence never overlays it. Running the
    scope inventory here means a source change during the scan fails
    the adapter call; the writer pulls the records through
    ``inventory.inventory_scope`` / ``inventory.inventory_all``.
    """
    runs = tuple(requested_runs)
    if runs:
        raise ContractError(
            "plan_corpus inventories the base plans corpus and takes no "
            f"run overlays ({len(runs)} requested)",
            code="plan-corpus-runs",
        )
    inventory.inventory_scope(ctx, SCOPE_ID)
    return AdapterResult()
