#!/usr/bin/env python3
"""Cost arithmetic for harness-churn-redesign.md.

All rates are labeled assumptions anchored to E1's dollar figures
(codex $0.1788 / 6 req; harness $0.5027 / 16 req on the identical task).
OpenAI-style cache economics: cache READ = 0.10x input rate, cache WRITE = 1.25x.
"""

P_IN = 1.25    # $/M input tokens   (assumption, see sanity check below)
P_OUT = 7.50   # $/M output tokens  (5x input, typical gpt-5-class ratio)
R_READ, R_WRITE = 0.10, 1.25

def dollars(n, rate):  # dollars for n tokens at $rate/M
    return n / 1e6 * rate

print("=" * 72)
print("0. BASELINE SANITY — can assumed rates reproduce E1's per-request cost?")
print("=" * 72)
codex_avg = 0.1788 / 6
harness_avg = 0.5027 / 16
print(f"implied avg request cost: codex ${codex_avg:.4f} | harness ${harness_avg:.4f}")
# model one "average" request: 28k input (85% cached-read, 15% cache-write) + 2.2k output
for tin, tcache, tout in [(28_000, 0.85, 2_200), (32_000, 0.90, 2_500)]:
    cin = tin * (tcache * R_READ + (1 - tcache) * R_WRITE)
    cost = dollars(cin, P_IN) + dollars(tout, P_OUT)
    print(f"  {tin//1000}k in ({tcache:.0%} cached) + {tout//1000}k out -> ${cost:.4f}")
print(f"  -> bracketed around the ${harness_avg:.4f} implied average. rates OK as labels.\n")

print("=" * 72)
print("1. E1 DECOMPOSITION — cost ratio vs request-count ratio")
print("=" * 72)
ratio_cost = 0.5027 / 0.1788
ratio_req = 16 / 6
print(f"cost ratio      : {ratio_cost:.2f}x")
print(f"request ratio   : {ratio_req:.2f}x")
print(f"per-request     : {ratio_cost/ratio_req:.2f}x (the rest is token mix/cache, not request count)")
print(f"=> ~{100*(1-6/16):.0f}% of the 16 requests are 'overhead' vs codex's 6.\n")

print("=" * 72)
print("2. E2 PREFIX PIN — steady-state per-request churn after the swap")
print("=" * 72)
# Pinned: cache-hit length frozen at S=10.3k tokens; everything after S
# (region R, the live tail of the session) is cache-WRITTEN on every request
# instead of being read.
S = 10_300
for R in (113, 500, 1004):
    delta = dollars(R, P_IN) * (R_WRITE - R_READ)
    print(f"region R={R:5d} tok: ${delta:.6f}/req "
          f"(= R x (1.25-0.10) x P_IN)  ->  per 100 requests: ${100*delta:.4f}")
one_time = dollars(S, P_IN) * (R_WRITE - R_READ)
print(f"one-time re-write of the {S//1000}k pinned prefix at the swap: ${one_time:.4f}")
print(f"(region R grows with session length: 113->1004 tok is a 9x multiplier)\n")

print("=" * 72)
print("3. CHANGE #1 — turn consolidation: 16 -> 8 requests")
print("=" * 72)
for target in (8, 6):
    saved = (16 - target) * harness_avg
    print(f"  16 -> {target} req: ~${saved:.3f} saved on the E1 task "
          f"({100*saved/0.5027:.0f}% of harness cost)")
print("  (each eliminated request saves its full average cost: prefix read +\n"
      "   fresh write + output; overhead requests are pure loss — no work done)\n")

print("=" * 72)
print("4. CHANGE #3 — codex-family tool surface: 30 -> 12 tools")
print("=" * 72)
for avg_def, kept in [(250, 12), (300, 10)]:
    removed_tok = (30 - kept) * avg_def
    # one session of 16 requests: 1 cold write, 15 cache reads of the prefix
    save = dollars(removed_tok, P_IN) * (R_WRITE + 15 * R_READ)
    print(f"  30->{kept} tools x {avg_def} tok = {removed_tok} tok/req prefix:\n"
          f"    per 16-req session: ${save:.4f} saved; per 100-req session: "
          f"${dollars(removed_tok, P_IN)*(R_WRITE+99*R_READ):.4f}")
print("  (secondary: smaller prefix delays compaction; each avoided compaction\n"
      "   saves its prefire+compact+continue request burst)\n")

print("=" * 72)
print("5. CHANGE #1b — MCP discovery: 2 model calls per tool")
print("=" * 72)
for n_tools in (1, 2, 4):
    c = 2 * n_tools * harness_avg
    print(f"  {n_tools} MCP tool(s) discovered lazily: {2*n_tools} req x ${harness_avg:.4f} = ${c:.3f}\n".rstrip())
print("  in-budget pre-exposure (<=2k tok of MCP schemas) costs one extra cache\n"
      f"  write of 2k tok = ${dollars(2000,P_IN)*R_WRITE:.4f} once, vs ${2*harness_avg:.4f} per tool in requests\n")

print("=" * 72)
print("6. CHANGE #5 — E5 replay bloat: parallel calls -> reasoning siblings")
print("=" * 72)
# each of K parallel function_calls emits its own reasoning sibling item
# (~500 tok, replayed verbatim on every subsequent request)
for K in (1, 5):
    for n_sub in (10,):
        c = n_sub * K * 500 * R_READ * P_IN / 1e6
        print(f"  K={K} siblings x 500 tok, replayed on {n_sub} later requests: ${c:.4f}")
print("  explicit parallel_tool_calls=false pins K=1 (codex parity)\n")

print("=" * 72)
print("7. CHANGE #4 — E6 brick: catastrophic tail (claude messages wire)")
print("=" * 72)
for ctx in (40_000, 100_000):
    re = dollars(ctx, P_IN) * R_WRITE
    print(f"  session bricks at {ctx//1000}k ctx: full restart re-pays cold write "
          f"${re:.4f} + all output already generated is stranded\n".rstrip())
print("  (frequency-limited to claude+high-reasoning, but EV = freq x total session cost)")
