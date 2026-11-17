---
name: tdd
description: Test-driven delivery for production Rust. The loop is red, then green, then refactor. Use when changing crates/ or prod/ behavior or fixing a bug.
---

# Test-driven delivery

This skill is the judgement the hook cannot make. Follow it on every cycle for production Rust under `crates/` or `prod/`. The project rule states the same order. Adapted from the supermatt `tdd` skill; see [NOTICE](NOTICE).

## Loop

Work one vertical slice at a time: one public behavior, one test, one minimal implementation. Do not write a batch of tests for imagined behavior and then implement them all.

1. **Name the break.** Before writing the test, name the production change that should turn it red. If you cannot name one, you do not have a behavior yet.
2. **Red.** Put the test in a sibling `*_tests.rs`, `*_test.rs`, `test_*.rs`, or a `tests/` file. Run `cargo test -p <crate>` (add a test filter when you know the name). Red is an assertion failure for that break. An error, a typo, a failed import, `error[E…]`, or `could not compile` is not red: fix the harness and run again. When the test names a function that does not exist, add a stub that compiles and returns the zero value, then run again until the assertion fails. A test that passes immediately pins behavior that is already there. Rewrite it to name a break that is not there yet, unless you are characterizing current behavior (below). If production code landed before the test, the cycle counts only after you undo that behavior, watch the assertion fail, and then put the behavior back.
3. **Green.** Add only the production code the failing assertion reaches. Another input, another branch, or a later rule waits for its own red test. During the cycle, run the new test and the other tests of that crate. `cargo test -p <crate>` is the gate for this repo; full-workspace builds are too slow to be the inner loop.
4. **Refactor.** After the affected tests are green, and only then, simplify. A refactor does not change assertions except for a mechanical rename or move. Run the affected tests after each step. Do not start a new behavior in the same edit.

A cycle is not a commit boundary. Several cycles can finish one behavior. Before handoff, run `cargo test -p <crate>` for every crate you changed and report each failure by name, including failures you did not cause.

## Tests worth keeping

Test through the crate's public interface, the way a caller would. A good test reads as a specification and survives an internal rewrite.

- Expected values come from an independent source: a literal, a worked example, or the spec. Do not recompute the expected value the way the implementation does.
- Assert the behavior that depends on a decision ("a failed call is tried 5 times, then stops"), not the constant that encodes it.
- Do not mock internal collaborators, call private helpers, or check a side channel the caller cannot see.
- Agree the public surface before adding tests. A seam named in the task is already agreed. When the change needs a new public surface, name it before writing the test.

## Characterization

When the behavior already exists and has no test, pin what it does now, including behavior that looks wrong. Assert a value you know is wrong, run the test, and copy the actual value into the assertion. That expected value comes from the code on purpose. Keep a wrong pinned value until a separate change fixes it. Before editing the covered code, break it in one small way, watch a test go red, and undo the break. If nothing goes red, the test does not cover the edit yet.

## After green

Break each production change once, in a small realistic way (wrong constant, wrong branch, missing empty check). The affected test must go red on an assertion, not on a compile error. Undo the break. If a mutation stays green and no caller can observe it, delete that code. This check is manual. The hook does not run it, and this factory does not ship the supermatt `mutate.sh` runner.

## What the hook enforces

`.cursor/hooks/tdd_loop.py` denies edits to production `.rs` files under `crates/` and `prod/`, and recognizable shell writes to those files, until this conversation has edited a test file and a later `cargo test` or `cargo nextest` run printed an assertion failure (`test result: FAILED` or a nextest `FAIL`). A compile error unlocks only a stub, not the rest of the implementation. After a real red, implementation is allowed. A passing run then unlocks refactor. Editing a test file again requires a new red before the next production change.

The hook cannot see inline `#[cfg(test)]` modules inside a production file, shell writes it does not recognize, or whether the assertion is the break you named. `afterFileEdit` runs too late to reject an edit. `sessionStart` is not wired: cloud agents do not run it. Prompt hooks are not used: cloud agents do not run them.

## Credits

The named break, the red and green rules, the change-detector and tautology warnings, and the mutation check are adapted from the supermatt `tdd` skill (Matt Pocock and Leonid Svyatov, MIT), which adapts Jesse Vincent's Superpowers test-driven-development skill (MIT). This file is a shortened project skill, not a copy of that tree.
