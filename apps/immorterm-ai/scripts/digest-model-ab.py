#!/usr/bin/env python3
"""
digest-model-ab.py — A/B quality benchmark: Sonnet vs Haiku digest extraction.

Replays real production digest transcripts (recovered from
~/.immorterm/wrapper-transcripts/) through both models using the EXACT
production extraction prompt, then has Opus judge the two outputs blind.

Each (section, model) extraction and each judge call runs through a warm
immorterm-p pool so the ~9.5K-char extraction prompt is cached across sections
(cuts the benchmark's own quota burn). Results are saved incrementally to
OUT_DIR so a mid-run quota cap-out loses nothing.

Usage:
  python3 digest-model-ab.py            # run full A/B (sonnet vs haiku, opus judge)
  python3 digest-model-ab.py --report   # re-print report from saved results, no model calls
"""
import json, os, re, subprocess, sys, random, time

HOME = os.path.expanduser("~")
IMPP = os.path.join(HOME, ".immorterm/bin/immorterm-p")
WT_DIR = os.path.join(HOME, ".immorterm/wrapper-transcripts")
OUT_DIR = os.path.join(HOME, ".immorterm/eval/digest-ab")
PROMPT_FILE = os.path.join(OUT_DIR, "extraction-prompt.txt")
RESULTS_FILE = os.path.join(OUT_DIR, "results.json")

# Real digest transcripts picked across a size spread (chars in the
# transcript_to_analyze block). Reproducible selection for this run.
SECTIONS = [
    "e6a25d64-f248-46d1-a6ab-13a0bdb78770.jsonl",  # ~2.1K
    "08fcefae-1ec4-43e5-8057-8be810fb006a.jsonl",  # ~4.9K
    "100329ed-4dd8-4b27-a7a0-3a656b6406e1.jsonl",  # ~6.2K
    "225589bd-9555-428a-9456-4de18c3746ac.jsonl",  # ~8.0K
    "c0914115-c2ae-4edb-b718-60da7d096606.jsonl",  # ~12K
    "5f3a88fa-4e56-4a36-9a89-b98f6ad6c79c.jsonl",  # ~23K
]

ARM_MODELS = {"sonnet": "sonnet", "haiku": "haiku"}
JUDGE_MODEL = "opus"

JUDGE_PROMPT = """You are a strict evaluator of memory-extraction quality for an AI coding-assistant memory system. You are given ONE developer/AI transcript, then TWO extractions of memories from it (labeled A and B), produced by two different models from the SAME prompt and SAME transcript.

A good extraction:
- ACCURACY: every memory is supported by the transcript; no hallucinated/invented facts.
- COMPLETENESS: captures the genuinely memorable facts (decisions, bugs+root causes, gotchas, conventions, preferences) without padding on routine/obvious things.
- ATOMICITY: one fact per memory; compound facts are split.
- SPECIFICITY: concrete and self-contained (names files, functions, concepts) rather than vague.
- SUMMARY: session_summary / title / at_a_glance are accurate and useful.

Score A and B on each dimension from 1 (poor) to 5 (excellent). Then pick the overall winner. Penalize hallucinations heavily — a confidently-wrong memory is worse than a missing one. More memories is NOT better if they are padding or redundant.

Output ONLY this JSON object (no prose, no fences), writing it to your output file:
{"A":{"accuracy":N,"completeness":N,"atomicity":N,"specificity":N,"summary":N},"B":{"accuracy":N,"completeness":N,"atomicity":N,"specificity":N,"summary":N},"winner":"A|B|tie","margin":"clear|slight|tie","rationale":"2-3 sentences citing specifics from the transcript"}"""


def load_prompt():
    with open(PROMPT_FILE) as f:
        return f.read()


def extract_transcript(jsonl_path):
    """Recover the DELIMITED_INPUT (the <transcript_to_analyze> block) that this
    digest fed to the model, from the saved wrapper transcript."""
    best = None
    with open(jsonl_path, errors="ignore") as f:
        for line in f:
            if "transcript_to_analyze" not in line:
                continue
            try:
                row = json.loads(line)
            except Exception:
                continue
            for s in _walk_strings(row):
                if "transcript_to_analyze" in s and (best is None or len(s) > len(best)):
                    best = s
    return best


def _walk_strings(o):
    if isinstance(o, dict):
        for v in o.values():
            yield from _walk_strings(v)
    elif isinstance(o, list):
        for v in o:
            yield from _walk_strings(v)
    elif isinstance(o, str):
        yield o


def run_impp(pool, model, system_prompt, stdin_text, timeout=320):
    """Invoke immorterm-p one call; return (raw_stdout, usage_dict, rc)."""
    usage_file = os.path.join(OUT_DIR, f"usage-{pool}-{int(time.time()*1000)}.json")
    err_file = usage_file + ".err"
    env = dict(os.environ)
    env["IMMORTERM_P_USAGE_FILE"] = usage_file
    # stderr → a real file, not a pipe: a detached child inheriting a copy of an
    # stderr PIPE would block communicate() on EOF. A file fd never blocks.
    try:
        with open(err_file, "w") as ef:
            proc = subprocess.run(
                [IMPP, "--pool", pool, "--model", model,
                 "--permission-mode", "bypassPermissions",
                 "--allowed-tools", "Write",
                 "--append-system-prompt", system_prompt],
                input=stdin_text, stdout=subprocess.PIPE, stderr=ef,
                text=True, env=env, timeout=timeout)
        rc = proc.returncode
        out = proc.stdout
        err = open(err_file).read() if os.path.exists(err_file) else ""
    except subprocess.TimeoutExpired:
        return "", {}, 124
    finally:
        if os.path.exists(err_file):
            os.remove(err_file)
    usage = {}
    if os.path.exists(usage_file):
        try:
            usage = json.load(open(usage_file))
        except Exception:
            pass
        os.remove(usage_file)
    if rc != 0 and err:
        sys.stderr.write(f"    [impp {model} rc={rc}] {err.strip()[:200]}\n")
    return out, usage, rc


def parse_json_blob(raw):
    """Strip optional fences and parse the model's JSON output."""
    if not raw or not raw.strip():
        return None
    s = raw.strip()
    s = re.sub(r"^```(?:json)?\s*\n?", "", s)
    s = re.sub(r"\n?```\s*$", "", s.strip())
    try:
        return json.loads(s)
    except Exception:
        # last resort: grab the outermost {...}
        m = re.search(r"\{.*\}", s, re.DOTALL)
        if m:
            try:
                return json.loads(m.group(0))
            except Exception:
                return None
        return None


def load_results():
    if os.path.exists(RESULTS_FILE):
        return json.load(open(RESULTS_FILE))
    return {"sections": {}}


def save_results(res):
    tmp = RESULTS_FILE + ".tmp"
    with open(tmp, "w") as f:
        json.dump(res, f, indent=2)
    os.replace(tmp, RESULTS_FILE)


def mem_count(extraction):
    if isinstance(extraction, dict) and isinstance(extraction.get("memories"), list):
        return len(extraction["memories"])
    return 0


def cleanup_pools():
    ai = os.path.join(HOME, ".immorterm/bin/immorterm-ai")
    for pool in ("evalsonnet", "evalhaiku", "evaljudge"):
        subprocess.run([ai, "-S", f"impp-pool-{pool}", "-X", "quit"],
                       capture_output=True)
        d = os.path.join(HOME, ".immorterm/pool", pool)
        meta = os.path.join(d, "meta.json")
        if os.path.exists(meta):
            try:
                rpid = json.load(open(meta)).get("reaper_pid")
                if rpid:
                    subprocess.run(["kill", str(rpid)], capture_output=True)
            except Exception:
                pass
        subprocess.run(["rm", "-rf", d], capture_output=True)


def run():
    os.makedirs(OUT_DIR, exist_ok=True)
    extraction_prompt = load_prompt()
    res = load_results()

    # ---- Extraction arms ----
    for fn in SECTIONS:
        path = os.path.join(WT_DIR, fn)
        if not os.path.exists(path):
            print(f"[skip] missing transcript {fn}")
            continue
        sid = fn.replace(".jsonl", "")
        sec = res["sections"].setdefault(sid, {})
        transcript = extract_transcript(path)
        if not transcript:
            print(f"[skip] no transcript_to_analyze in {fn}")
            continue
        sec["chars"] = len(transcript)
        for arm, model in ARM_MODELS.items():
            if sec.get(arm, {}).get("ok"):
                print(f"[cached] {sid[:8]} {arm}")
                continue
            print(f"[run] {sid[:8]} ({len(transcript)} chars) -> {arm}")
            t0 = time.time()
            raw, usage, rc = run_impp(f"eval{arm}", model, extraction_prompt, transcript)
            parsed = parse_json_blob(raw)
            sec[arm] = {
                "ok": rc == 0 and parsed is not None,
                "rc": rc,
                "elapsed_s": round(time.time() - t0, 1),
                "mem_count": mem_count(parsed),
                "cost_usd": usage.get("cost_usd", 0),
                "output_tokens": usage.get("output_tokens", 0),
                "cache_creation": usage.get("cache_creation_input_tokens", 0),
                "cache_read": usage.get("cache_read_input_tokens", 0),
                "extraction": parsed,
            }
            sec["transcript"] = transcript
            save_results(res)

    # ---- Judge (blind, randomized A/B order) ----
    rnd = random.Random(1337)  # fixed seed: reproducible A/B assignment
    for sid, sec in res["sections"].items():
        if sec.get("judge", {}).get("ok"):
            print(f"[cached] judge {sid[:8]}")
            continue
        if not (sec.get("sonnet", {}).get("ok") and sec.get("haiku", {}).get("ok")):
            print(f"[skip judge] {sid[:8]} — an arm failed")
            continue
        # Randomly assign which model is A vs B to remove position bias.
        if rnd.random() < 0.5:
            a_model, b_model = "sonnet", "haiku"
        else:
            a_model, b_model = "haiku", "sonnet"
        a_ext = json.dumps(sec[a_model]["extraction"], indent=2)
        b_ext = json.dumps(sec[b_model]["extraction"], indent=2)
        judge_input = (
            "=== TRANSCRIPT ===\n" + sec["transcript"] +
            "\n\n=== EXTRACTION A ===\n" + a_ext +
            "\n\n=== EXTRACTION B ===\n" + b_ext
        )
        print(f"[judge] {sid[:8]}  (A={a_model}, B={b_model})")
        raw, usage, rc = run_impp("evaljudge", JUDGE_MODEL, JUDGE_PROMPT, judge_input)
        verdict = parse_json_blob(raw)
        sec["judge"] = {
            "ok": rc == 0 and verdict is not None,
            "rc": rc,
            "a_model": a_model,
            "b_model": b_model,
            "verdict": verdict,
            "cost_usd": usage.get("cost_usd", 0),
        }
        save_results(res)

    cleanup_pools()
    report(res)


def report(res):
    print("\n" + "=" * 78)
    print("DIGEST MODEL A/B — Sonnet vs Haiku  (judge: opus, blind)")
    print("=" * 78)
    hdr = f"{'section':10} {'chars':>6} | {'sonnet':>22} | {'haiku':>22} | winner"
    print(hdr); print("-" * len(hdr))
    agg = {"sonnet": 0, "haiku": 0, "tie": 0}
    cost = {"sonnet": 0.0, "haiku": 0.0, "judge": 0.0}
    dims = ["accuracy", "completeness", "atomicity", "specificity", "summary"]
    dimtot = {"sonnet": {d: [] for d in dims}, "haiku": {d: [] for d in dims}}
    for sid, sec in res["sections"].items():
        s, h = sec.get("sonnet", {}), sec.get("haiku", {})
        def cell(x):
            if not x.get("ok"):
                return f"FAIL(rc={x.get('rc','?')})"
            return f"{x['mem_count']}mem {x['elapsed_s']}s ${x['cost_usd']:.3f}"
        cost["sonnet"] += s.get("cost_usd", 0) or 0
        cost["haiku"] += h.get("cost_usd", 0) or 0
        j = sec.get("judge", {})
        win = "?"
        if j.get("ok") and j.get("verdict"):
            v = j["verdict"]
            cost["judge"] += j.get("cost_usd", 0) or 0
            w = v.get("winner", "?")
            # map A/B back to model
            if w == "A": win = j["a_model"]
            elif w == "B": win = j["b_model"]
            elif w == "tie": win = "tie"
            if win in agg: agg[win] += 1
            win += f" ({v.get('margin','?')})"
            # collect dimension scores per model
            for letter, model in (("A", j["a_model"]), ("B", j["b_model"])):
                sc = v.get(letter, {})
                for d in dims:
                    if isinstance(sc.get(d), (int, float)):
                        dimtot[model][d].append(sc[d])
        print(f"{sid[:10]:10} {sec.get('chars',0):>6} | {cell(s):>22} | {cell(h):>22} | {win}")
    print("-" * len(hdr))
    print(f"\nWINS — sonnet: {agg['sonnet']}   haiku: {agg['haiku']}   tie: {agg['tie']}")
    print(f"COST (this run) — sonnet: ${cost['sonnet']:.3f}  haiku: ${cost['haiku']:.3f}  judge: ${cost['judge']:.3f}")
    # avg dimension scores
    print("\nAVG JUDGE SCORES (1-5):")
    print(f"  {'dim':14} {'sonnet':>8} {'haiku':>8}")
    for d in dims:
        sv = dimtot["sonnet"][d]; hv = dimtot["haiku"][d]
        sa = sum(sv)/len(sv) if sv else 0
        ha = sum(hv)/len(hv) if hv else 0
        print(f"  {d:14} {sa:>8.2f} {ha:>8.2f}")
    # cost extrapolation
    n = sum(1 for s in res["sections"].values() if s.get("sonnet", {}).get("ok"))
    if n:
        savg = cost["sonnet"]/n; havg = cost["haiku"]/n
        print(f"\nPER-DIGEST avg — sonnet ${savg:.4f}  haiku ${havg:.4f}  "
              f"(haiku is {savg/havg:.1f}x cheaper)" if havg else "")
    print("\nRationales:")
    for sid, sec in res["sections"].items():
        j = sec.get("judge", {})
        if j.get("ok") and j.get("verdict"):
            print(f"  [{sid[:8]}] {j['verdict'].get('rationale','')}")
    print(f"\nFull results: {RESULTS_FILE}")


if __name__ == "__main__":
    if "--report" in sys.argv:
        report(load_results())
    else:
        run()
