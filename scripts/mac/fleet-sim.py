#!/usr/bin/env python3
"""Thirty days of a 15-car Bengaluru taxi fleet on a phone running agenticOS.

    python3 scripts/mac/fleet-sim.py [--out target/fleet-sim] [--seed 7] [--days 30]

Each day the fleet's trips, shifts, fuel and charging bills and cash are invented
(crates/fleet/src/gen.rs) with problems planted at known places; the daemon takes them in and
its checks are scored against what was planted. The owner then closes the day in fixed phrases:
takes in riders' cash, pays each driver with the approval key. Uber pays weekly (once short,
which is claimed), a Claude client reads over MCP on day 10, and the phone restarts on day 15.
At the end: each car's month, electric against CNG, and the plan. Results go to
<out>/results.json. The fleet and its drivers are made up; every figure in the profile is an
assumption.
"""
import argparse, json, os, shutil, signal, socket, subprocess, sys, time, urllib.request
from datetime import date, timedelta

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
ap = argparse.ArgumentParser()
ap.add_argument("--out", default=os.path.join(ROOT, "target", "fleet-sim"))
ap.add_argument("--seed", type=int, default=7)
ap.add_argument("--days", type=int, default=30)
ap.add_argument("--first", default="2026-10-14")
A = ap.parse_args()
OUT = os.path.abspath(A.out)
PHONE, AGENTD, PORT = os.path.join(OUT, "phone"), os.path.join(ROOT, "target", "release", "agentd"), 8773
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
    s.settimeout(600)
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
        print(f"    CHECK FAILED {when} {name}: {str(detail)[:240]}")


def with_key(text):
    ans = ctl("chat " + text)
    if "approval required" in ans:
        ctl("press " + ans.split("(id ")[1].split(")")[0])
        ans = ctl("chat " + text)
    return ans


def state():
    return json.loads(ctl("fleet state"))


rupees = lambda p: f"{p / 100:.2f}".rstrip("0").rstrip(".")

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
fleet_profile = "{ROOT}/examples/fleet/profile.toml"
''')
TOKEN = subprocess.run([AGENTD, "init", "-c", CFG], capture_output=True, text=True).stdout.strip().splitlines()[-1]
start()

first = date.fromisoformat(A.first)
rows, totals, wall0 = [], {"planted": 0, "found": 0, "missed": [], "extra": [], "refused": [], "by_kind": {}}, time.time()
cash_recovered = paid_out = 0
for i in range(A.days):
    day = (first + timedelta(days=i)).isoformat()
    sc = json.loads(ctl(f"fleet sim {day} {day} {A.seed}"))
    for k in ("planted", "found"):
        totals[k] += sc[k]
    for k in ("missed", "extra", "refused"):
        totals[k] += sc[k]
    for k, v in sc["by_kind"].items():
        t = totals["by_kind"].setdefault(k, [0, 0])
        t[0] += v["planted"]
        t[1] += v["found"]
    check(day, "every planted problem found, nothing else reported, no event refused", sc["planted"] == sc["found"] and not sc["missed"] and not sc["extra"] and not sc["refused"], sc)
    ctl(f"fleet clock {day}T22:30")
    st = state()
    notes = []
    # ---- Uber pays for the week before, two days after it ends; the second week comes short
    week, dow = divmod(i, 7)
    if dow == 1 and week >= 1:
        a, b = (first + timedelta(days=7 * (week - 1))).isoformat(), (first + timedelta(days=7 * week - 1)).isoformat()
        cut = 2400 if week == 2 else 0
        ctl(f"fleet payout uber {a} {b} {cut}")
        notes.append(f"Uber paid for {a}..{b}" + (" (₹2,400 short)" if cut else ""))
        can = next(p["claimable"] for p in state()["platforms"] if p["name"] == "Uber")
        check(day, "the payout reconciles to the rupee" if not cut else "the short payout is seen exactly", can == cut * 100, can)
        if can:
            said = ctl("chat fleet issues")
            check(day, "the owner is told Uber paid short", "Uber paid ₹2,400 less" in said, said)
            ans = with_key(f"fleet claim {rupees(can)} from uber")
            check(day, "the exact shortfall claimed with the key", "CLM-UBER" in ans, ans)
            notes.append("claimed ₹2,400 from Uber with the key")
    # ---- a Claude client connected over MCP reads the fleet and is refused the money
    if i == 9:
        ok_all = all(mcp(t, {})[0] for t in ["fleet_dashboard", "fleet_cars", "fleet_drivers", "fleet_issues", "fleet_payouts", "fleet_plan"])
        ok, out = mcp("fleet_pay_driver", {"driver": "D01", "amount": "5000"})
        check(day, "an MCP client reads the fleet and cannot pay a driver", ok_all and not ok and "approval required" in out, out)
        ctl("decline " + out.split("(id ")[1].split(")")[0])
        notes.append("a Claude client read the fleet over MCP; its request to pay a driver waited for the key and was declined")
    if i == 14:
        before = state()
        stop()
        start()
        check(day, "the book survives a restart", state() == before)
        notes.append("the phone restarted; the book and the generator carried on")
    # ---- the owner closes the day: riders' cash in, each driver paid with the key
    kinds = {}
    for x in st["issues"]:
        kinds[x["kind"]] = kinds.get(x["kind"], 0) + 1
    for d in st["drivers"]:
        if d["cash_held"] > 0:
            ans = ctl(f"chat driver cash {rupees(d['cash_held'])} from {d['id']}")
            check(day, "cash kept back is taken in", "still holds ₹0" in ans, ans)
            cash_recovered += d["cash_held"]
        if d["due"] > 0:
            ans = with_key(f"pay {rupees(d['due'])} to driver {d['id']}")
            check(day, "driver paid exactly the sheet", "recorded as handed" in ans, ans)
            paid_out += d["due"]
    for x in st["issues"]:
        if x["kind"] == "service_due":
            ctl("fleet event " + json.dumps({"t": "expense", "car": x["who"], "at": f"{day}T22:40", "amount": 4200 if x["who"] < "C10" else 2800, "note": "service"}))
            notes.append(f"{x['who']} was due a service: booked and recorded")
    left = [d for d in state()["drivers"] if d["due"] != 0 or d["cash_held"] != 0]
    check(day, "every driver settled to zero", not left, left)
    net = sum(c["net"] for c in st["cars"])
    rows.append({"day": day, "events": sc["events"], "planted": sc["planted"], "found": sc["found"], "by_kind": sc["by_kind"], "issues": kinds,
                 "cars_out": sum(1 for c in st["cars"] if c["trips"]), "trips": sum(c["trips"] for c in st["cars"]), "net": net,
                 "energy": sum(c["energy"] for c in st["cars"]), "notes": notes, "seconds": sc["seconds"]})
    print(f"{day}: {rows[-1]['cars_out']:>2} cars, {rows[-1]['trips']:>3} trips, keeps ₹{net / 100:>8,.0f}; planted {sc['planted']} found {sc['found']}"
          + (f" {dict((k, v['planted']) for k, v in sc['by_kind'].items())}" if sc["planted"] else "") + ("; " + "; ".join(notes) if notes else ""))

last = (first + timedelta(days=A.days - 1)).isoformat()
month_text = ctl("chat fleet month 2026-1").replace("done: ", "", 1)
plan_text = ctl("chat fleet plan").replace("done: ", "", 1)
payouts_text = ctl("chat fleet payouts").replace("done: ", "", 1)
st = state()
hits, size = [], 0
for dirpath, _, files in os.walk(os.path.join(PHONE, "data", "fleet")):
    for f in files:
        blob = open(os.path.join(dirpath, f), "rb").read()
        size += len(blob)
        if any(x in blob for x in [b"Koramangala", b"Manjunath", b"Uber", b"T000"]):
            hits.append(f)
check(last, "the fleet's book is not readable on disk", not hits, hits[:5])
v = ctl("verify")
check(last, "the ledger verifies", "ledger ok" in v, v)
stop()
json.dump({"days": rows, "totals": totals, "checks": checks, "month_text": month_text, "plan_text": plan_text, "payouts_text": payouts_text,
           "cash_recovered": cash_recovered, "paid_out": paid_out, "disk_bytes": size, "seconds": time.time() - wall0, "state": st},
          open(os.path.join(OUT, "results.json"), "w"), indent=1)
open(os.path.join(OUT, "month.txt"), "w").write(month_text + "\n\n" + plan_text + "\n\n" + payouts_text + "\n")
bad = [c for c in checks if not c["ok"]]
print(f"\n{sum(r['trips'] for r in rows):,} trips over {len(rows)} days; {totals['planted']} problems planted, {totals['found']} found "
      f"({', '.join(f'{k} {v[1]}/{v[0]}' for k, v in sorted(totals['by_kind'].items()))}); {len(totals['extra'])} false alarms; "
      f"{len(checks) - len(bad)} of {len(checks)} checks passed; {size / 1e6:.1f} MB on the phone; {time.time() - wall0:.0f}s")
for c in bad:
    print("  FAILED", c["when"], c["name"], "—", c["detail"][:200])
print()
print(month_text)
print()
print(plan_text)
