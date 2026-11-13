#!/usr/bin/env python3
"""A financial year of a 5,000-bills-a-day store on a phone running agenticOS.

    python3 scripts/mac/store-sim.py [--out target/store-sim] [--seed 7]

For each month of FY 2025-26 the tills' bills are invented (crates/retail/src/gen.rs) with
faults planted at known places, audited by the daemon, and the audit is scored against what was
planted. A Claude client connected over MCP (as the Claude app on the owner's Android phone
would be) reads the audit and proposes fixes; the owner applies them and files the month's GST
return with the approval key. After March the year is tallied and the income-tax computation
lodged. Results go to <out>/results.json. The store, its suppliers and its shoppers are made up.
"""
import argparse, json, os, shutil, signal, socket, subprocess, sys, time, urllib.request
from datetime import date, timedelta

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
ap = argparse.ArgumentParser()
ap.add_argument("--out", default=os.path.join(ROOT, "target", "store-sim"))
ap.add_argument("--seed", type=int, default=7)
ap.add_argument("--months", type=int, default=12)
A = ap.parse_args()
OUT = os.path.abspath(A.out)
PHONE, AGENTD, PORT = os.path.join(OUT, "phone"), os.path.join(ROOT, "target", "release", "agentd"), 8772
CFG, SOCK = os.path.join(PHONE, "agentd.toml"), os.path.join(PHONE, "c.sock")
os.environ["AGENTD_PASSPHRASE"] = "sim"
proc = None


def start():
    global proc
    proc = subprocess.Popen([AGENTD, "run", "-c", CFG], stdout=open(os.path.join(OUT, "agentd.log"), "a"), stderr=subprocess.STDOUT)
    for _ in range(100):
        try:
            if ctl("stage"):
                return
        except OSError:
            time.sleep(0.1)
    sys.exit("agentd did not start; see agentd.log")


def stop():
    proc.send_signal(signal.SIGTERM)
    proc.wait(timeout=30)


def ctl(line):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(1800)
    s.connect(SOCK)
    s.sendall((line + "\n").encode())
    buf = b""
    while not buf.endswith(b"\n"):
        chunk = s.recv(1 << 16)
        if not chunk:
            break
        buf += chunk
    s.close()
    return buf.decode().rstrip("\n").replace("\x01", "\n")


def mcp(tool, args):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}).encode()
    req = urllib.request.Request(f"http://127.0.0.1:{PORT}/mcp", data=body, headers={"Authorization": "Bearer " + TOKEN})
    r = json.loads(urllib.request.urlopen(req, timeout=120).read())["result"]
    return (not r.get("isError", False)), r["content"][0]["text"]


checks = []


def check(when, name, ok, detail=""):
    checks.append({"when": when, "name": name, "ok": bool(ok), "detail": str(detail)[:300]})
    if not ok:
        print(f"    CHECK FAILED {name}: {str(detail)[:240]}")


def with_key(text):
    """The owner asks for something that needs the approval key, presses it, and asks again."""
    ans = ctl("chat " + text)
    if "approval required" in ans:
        ctl("press " + ans.split("(id ")[1].split(")")[0])
        ans = ctl("chat " + text)
    return ans


# what the client proposes for each kind of exception, in order (the first that applies to an
# exception wins; a credit note only fits tax that was over-charged)
POLICY = {
    "wrong_rate": ["credit_note", "absorb"], "slab_on_tag": ["credit_note", "absorb"], "old_rate": ["credit_note", "absorb"],
    "tax_arithmetic": ["recompute"], "total_mismatch": ["recompute"], "payment_mismatch": ["recompute"],
    "duplicate_no": ["renumber"], "gap": ["record_void"], "hsn_missing": ["set_from_catalog"], "hsn_wrong": ["set_from_catalog"],
    "gstin_invalid": ["treat_as_b2c"], "place_of_supply": ["reissue"], "return_unmatched": ["hold_refund"], "return_excess": ["hold_refund"],
    "cash_short": ["investigate"],
}

if os.path.exists(PHONE):
    shutil.rmtree(PHONE)
os.makedirs(PHONE, exist_ok=True)
open(os.path.join(OUT, "agentd.log"), "w").close()
open(CFG, "w").write(f'''data_dir = "{PHONE}/data"
listen = "127.0.0.1:{PORT}"
control_socket = "{SOCK}"
phone = "sim"
kdf_mib = 8
idle_secs = 86400
store_profile = "{ROOT}/examples/retail/profile.toml"
''')
TOKEN = subprocess.run([AGENTD, "init", "-c", CFG], capture_output=True, text=True).stdout.strip().splitlines()[-1]
start()

months, first = [], date(2025, 4, 1)
for k in range(A.months):
    y, m = first.year + (first.month - 1 + k) // 12, (first.month - 1 + k) % 12 + 1
    months.append((date(y, m, 1), (date(y + (m == 12), m % 12 + 1, 1) - timedelta(days=1))))
rows, wall0 = [], time.time()
for a, b in months:
    mo = a.isoformat()[:7]
    score = json.loads(ctl(f"shop sim {a} {b} {A.seed}"))
    check(mo, "every planted fault found, nothing else reported", score["planted"] == score["found"] and not score["missed"] and not score["extra"], score)
    # ---- the Claude client on the owner's phone, over MCP: read, propose, be refused the rest
    ok, audit = mcp("shop_audit", {"period": mo})
    kinds = [l.split()[0] for l in audit.splitlines()[1:] if l.startswith("  ")]
    proposed = 0
    for kind in kinds:
        for fix in POLICY[kind]:
            ok, out = mcp("shop_propose_fix", {"period": mo, "kind": kind, "fix": fix})
            if ok:
                proposed += int(out.split()[0])
    ok, out = mcp("shop_propose_fix", {"period": mo, "kind": kinds[0], "fix": "delete_the_bills"})
    check(mo, "a fix that does not fit is refused", not ok and "is not a fix" in out, out)
    ok, out = mcp("shop_apply_fixes", {"period": mo})
    check(mo, "the client cannot apply its own proposals", not ok and "approval required" in out, out)
    ctl("decline " + out.split("(id ")[1].split(")")[0])
    before = json.loads(ctl("shop state"))["months"][-1]
    check(mo, "proposals changed nothing", before["credit_notes"] == 0 and before["open"] > 0, before)
    if mo == "2025-09":
        stop()
        start()
        check(mo, "the books survive a restart", json.loads(ctl("shop state"))["months"][-1] == before)
    # ---- the owner: apply with the key, then file on the 11th of the next month
    done = with_key(f"apply fixes for {mo}")
    check(mo, "fixes applied with the key", "applied in" in done, done)
    ctl(f"shop clock {(b + timedelta(days=11)).isoformat()}T10:00")
    filed = with_key(f"file store gst for {mo}")
    check(mo, "the month's return lodged", "lodged with the simulated portal" in filed, filed)
    again = with_key(f"file store gst for {mo}")
    check(mo, "a second filing is refused", "already filed" in again, again)
    st = json.loads(ctl("shop state"))["months"][-1]
    check(mo, "no exception left open", st["open"] == 0, st)
    rows.append({"month": mo, "score": score, "proposed": proposed, **st})
    print(f"{mo}: {score['bills']:>7,} bills in {score['seconds']:5.1f}s; faults {score['found']}/{score['planted']}; "
          f"output ₹{st['output'] / 1e9:5.2f} cr, credit ₹{st['itc'] / 1e9:5.2f} cr, cash ₹{st['cash'] / 1e9:5.2f} cr; "
          f"short ₹{st['short_charged'] / 100:,.0f}, over ₹{st['over_charged'] / 100:,.0f}, credit notes ₹{st['credit_notes'] / 100:,.0f}")

ctl("shop clock 2026-04-20T10:00")
year_text = ctl("chat year tally").replace("done: ", "", 1)
itr = with_key("file income tax")
check("year", "income-tax computation lodged", "lodged with the simulated portal" in itr, itr)
st = json.loads(ctl("shop state"))
check("year", "the year's turnover is the months' added up", st["turnover"] == sum(r["taxable"] for r in rows), st["turnover"])
check("year", "every month filed", not st["unfiled"], st["unfiled"])
check("year", "money received differs from money billed by less than 0.01%", abs(st["gross_billed"] - st["received"]) < st["gross_billed"] / 10000, st["gross_billed"] - st["received"])
ok, client_year = mcp("shop_year", {})
check("year", "the client sees the same year as the owner", client_year == year_text)
hits, size = [], 0
for dirpath, _, files in os.walk(PHONE):
    for f in files:
        path = os.path.join(dirpath, f)
        if not os.path.isfile(path):
            continue
        blob = open(path, "rb").read()
        size += len(blob)
        if b"90000" in blob:
            hits.append(os.path.relpath(path, PHONE))
check("year", "no shopper's number on disk", not hits, hits[:5])
v = ctl("verify")
check("year", "the ledger verifies", "ledger ok" in v, v)
stop()
planted = sum(r["score"]["planted"] for r in rows)
fam = {}
for r in rows:
    for k, x in r["score"]["by_family"].items():
        fam.setdefault(k, [0, 0])
        fam[k][0] += x["planted"]
        fam[k][1] += x["found"]
json.dump({"months": rows, "year": st, "year_text": year_text, "checks": checks, "by_family": fam, "disk_bytes": size,
           "seconds": time.time() - wall0}, open(os.path.join(OUT, "results.json"), "w"), indent=1)
open(os.path.join(OUT, "year.txt"), "w").write(year_text + "\n")
bad = [c for c in checks if not c["ok"]]
print(f"\n{st['bills']:,} bills over {st['days']} days; {planted:,} faults planted, {sum(r['score']['found'] for r in rows):,} found; "
      f"audit {st['audit_ms']:.0f} ms a day; {size / 1e6:.1f} MB on the phone; {len(checks) - len(bad)} of {len(checks)} checks passed")
for c in bad:
    print("  FAILED", c["when"], c["name"], "—", c["detail"][:200])
print()
print(year_text)
