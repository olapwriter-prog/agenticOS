#!/usr/bin/env python3
"""Fifteen days in the life of a cafe owner whose phone runs agenticOS.

    python3 scripts/mac/cafe-sim.py [--days 15] [--model http://127.0.0.1:8091] [--out target/cafe-sim]

It starts a fresh agentd (simulated phone, the cafe in examples/cafe/profile.toml), imports a
month of past books, then plays 15 days: orders arrive as the channels would deliver them, the
owner talks to the phone in his own words three times a day, approves money with the key, and
corrects the phone when it misunderstands. Each day has one thing going wrong (rain, a short
payout, a stock-out, duplicate webhooks, a restart, a nosy client...).

Everything is measured from the running daemon: how each request was understood (fixed phrase,
learned shortcut, classifier, model), how long it took, whether it was right, what the
predictive cache had ready, and whether the business and privacy checks hold. Results go to
<out>/results.json; nothing here is asserted in advance.

The shop, its customers and its orders are invented by a seeded generator (crates/business).
"""
import argparse, json, os, random, shutil, signal, socket, subprocess, sys, time, urllib.request
from datetime import date, timedelta

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
ap = argparse.ArgumentParser()
ap.add_argument("--days", type=int, default=15)
ap.add_argument("--first", default="2026-10-30")
ap.add_argument("--history", type=int, default=29)
ap.add_argument("--model", default="http://127.0.0.1:8091", help="llama-server URL; '' for no model")
ap.add_argument("--out", default=os.path.join(ROOT, "target", "cafe-sim"))
ap.add_argument("--seed", type=int, default=7)
A = ap.parse_args()

A.out = os.path.abspath(A.out)
OUT, PHONE, EVENTS = A.out, os.path.join(A.out, "phone"), os.path.join(A.out, "events")
AGENTD = os.path.join(ROOT, "target", "release", "agentd")
CFG, SOCK, PORT = os.path.join(PHONE, "agentd.toml"), os.path.join(PHONE, "c.sock"), 8770
os.environ["AGENTD_PASSPHRASE"] = "sim"
rng = random.Random(A.seed)


def model_up():
    if not A.model:
        return False
    try:
        return urllib.request.urlopen(A.model + "/health", timeout=3).status == 200
    except Exception:
        return False


HAVE_MODEL = model_up()
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
    proc.wait(timeout=20)


def ctl(line):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(600)
    s.connect(SOCK)
    s.sendall((line.replace("\n", " ") + "\n").encode())
    buf = b""
    while not buf.endswith(b"\n"):
        chunk = s.recv(65536)
        if not chunk:
            break
        buf += chunk
    s.close()
    return buf.decode().rstrip("\n").replace("\x01", "\n")


def mcp(tool, args):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}).encode()
    req = urllib.request.Request(f"http://127.0.0.1:{PORT}/mcp", data=body, headers={"Authorization": "Bearer " + TOKEN})
    r = json.loads(urllib.request.urlopen(req, timeout=60).read())["result"]
    return (not r.get("isError", False)), r["content"][0]["text"]


def state():
    return json.loads(ctl("biz state"))


def metrics(reset=False):
    return json.loads(ctl("metrics reset" if reset else "metrics"))


# ---------------------------------------------------------------- the owner
# need -> (tool, the phone's fixed phrase, how this owner says it, other ways he says it, ways he
# has never used before the last days)
NEEDS = {
    "dashboard": ("biz_dashboard", "dashboard", "how did we do today",
                  ["ivala business ela undi", "show me today's sales", "sales so far"],
                  ["give me the day's summary", "what's the score today"]),
    "issues": ("biz_issues", "what needs me", "anything i should worry about",
               ["any problems", "emaina problems unnaya", "what needs my attention"],
               ["is anything stuck", "what went wrong today"]),
    "plan": ("biz_plan", "plan for tomorrow", "what do i cook tomorrow",
             ["repu plan cheppu", "shopping list for tomorrow", "how many plates tomorrow"],
             ["what should the kitchen prepare tomorrow", "tomorrow's buying list"]),
    "riders": ("biz_riders", "rider settlement", "how much for the boys today",
               ["delivery boys ki entha ivvali", "rider earnings", "what do ravi and suresh get"],
               ["what do i pay the delivery boys tonight", "boys' account for today"]),
    "payouts": ("biz_payouts", "platform payouts", "did swiggy and zomato pay properly",
                ["platform money status", "what do the apps owe me"],
                ["are the platform payments right", "check swiggy zomato settlements"]),
    "gst": ("biz_gst", "gst for this month", "gst so far this month",
            ["how much tax this month", "how much gst do i owe"],
            ["what is my gst bill so far", "tax position this month"]),
    "orders": ("biz_orders", "today's orders", "what orders are running",
               ["show orders", "open orders"],
               ["which orders are still going", "list what came in"]),
    "privacy": ("biz_privacy", "privacy report", "who has seen my customer data",
                ["what left the phone today"],
                ["is my customers' data safe", "what did we share today"]),
}
confirmed, used = set(), set()
requests, checks, corrections = [], [], []


def check(day, name, ok, detail=""):
    checks.append({"day": day, "name": name, "ok": bool(ok), "detail": str(detail)[:300]})
    if not ok:
        print(f"    CHECK FAILED {name}: {str(detail)[:200]}")


def say(day, when, text, expect, note=""):
    """One thing the owner says. Returns the phone's answer (after any approval)."""
    t0 = time.time()
    ans = ctl("chat " + text)
    wall = (time.time() - t0) * 1000
    m = metrics()["last"]
    rec = {"day": day, "when": when, "text": text, "expect": expect, "tool": m["tool"], "source": m["source"],
           "ms": round(m["ms"], 3), "wall_ms": round(wall, 1), "new": text not in used, "note": note, "answer": ans[:160]}
    used.add(text)
    gave_up = "could not turn that into" in ans or ans.startswith("blocked")
    gave_up = gave_up or ans.startswith("not done: failed") or ans.startswith("not done: no working")
    rec["ok"] = (not gave_up) and (expect is None or m["tool"] == expect)
    requests.append(rec)
    return ans, rec


def ask(day, when, need, novel=False):
    """The owner wants something and says it his way; he confirms or corrects the phone."""
    tool, fixed, habit, variants, never = NEEDS[need]
    if novel:
        text = rng.choice(never)
    else:
        x = rng.random()
        text = habit if (x < 0.6 or habit not in used) else (rng.choice(variants) if x < 0.88 else fixed)
    ans, rec = say(day, when, text, tool)
    if rec["ok"]:
        if text not in confirmed and rec["source"].split("+")[0] in ("model", "classifier"):
            say(day, when, "that was right", None, "confirms")
            confirmed.add(text)
        return ans
    # wrong, or the phone gave up: say so, then say it the way the phone knows
    if "approval required" in ans:
        ctl("decline " + ans.split("(id ")[1].split(")")[0])
    say(day, when, "that was wrong", None, "corrects")
    fixed_ans, r2 = say(day, when, fixed, tool, "correction")
    corrections.append({"day": day, "said": text, "got": rec["tool"] or rec["source"], "wanted": tool})
    confirmed.add(text)
    return fixed_ans


def approve(day, when, text, expect):
    """A request that needs the key: ask, press, ask again. Returns the final answer."""
    ans, rec = say(day, when, text, expect)
    if "approval required" in ans and rec["tool"] == expect:
        ctl("press " + ans.split("(id ")[1].split(")")[0])
        ans, _ = say(day, when, text, expect, "after the key")
    elif "approval required" in ans:
        ctl("decline " + ans.split("(id ")[1].split(")")[0])
    return ans, rec


# ---------------------------------------------------------------- a day
def play(day, lines):
    path = os.path.join(EVENTS, f"chunk-{day}.jsonl")
    open(path, "w").write("\n".join(lines) + "\n")
    t0 = time.time()
    out = ctl("biz play " + path)
    timing["play_s"] += time.time() - t0
    timing["events"] += len(lines)
    handled, refused = int(out.split()[0]), int(out.split(",")[1].split()[0])
    return handled, refused, out


def closing(day, novel=False):
    """The owner's closing routine: numbers, riders (cash in, earnings out), tomorrow."""
    when = "22:45"
    ctl(f"biz clock {day}T{when}")
    dash = ask(day, when, "dashboard", novel)
    # a prepared answer must equal a freshly computed one
    ok, fresh = mcp("biz_dashboard", {})
    check(day, "prepared dashboard equals a fresh one", dash.replace("done: ", "", 1) == fresh, dash[:80])
    ask(day, when, "riders", novel)
    for r in state()["riders"]:
        name = r["name"].lower()
        if r["cash_held"] > 0:
            ans, _ = say(day, when, rng.choice([f"took cash from {name}", f"{name} gave the cod cash"]), "biz_cash_in")
            if "handed over" not in ans:
                say(day, when, "that was wrong", None, "corrects")
                ans, _ = say(day, when, f"cash {r['cash_held'] / 100:g} from {name}", "biz_cash_in", "correction")
                corrections.append({"day": day, "said": "took cash from " + name, "got": "?", "wanted": "biz_cash_in"})
            check(day, f"{name}'s cash taken in", "handed over" in ans, ans)
        if r["due"] > 0:
            phrase = f"settle {name}"
            ans, rec = approve(day, when, phrase, "biz_pay_rider")
            if "recorded as handed" not in ans:
                issues_found.append({"day": day, "what": f"'{phrase}' did not pay: {ans[:140]}", "source": rec["source"]})
                ans, _ = approve(day, when, f"pay {r['due'] / 100:g} to rider {name}", "biz_pay_rider")
            check(day, f"{name} paid exactly what the sheet says", "recorded as handed" in ans, ans)
    left = [r for r in state()["riders"] if r["due"] != 0 or r["cash_held"] != 0]
    check(day, "riders settled to zero", not left, left)
    ask(day, when, "plan", novel)


issues_found = []
timing = {"play_s": 0.0, "events": 0}


def run_day(i, day):
    wd = date.fromisoformat(day).strftime("%A")
    sc = SCENARIOS.get(i, {})
    print(f"== day {i + 1}: {wd} {day}  {sc.get('name', '')}")
    path = os.path.join(EVENTS, f"{day}.jsonl")
    ctl(f"biz gen {day} {A.seed} {sc.get('scale', 1.0)} {sc.get('direct', 0)} {path}")
    ev = [l for l in open(path).read().splitlines() if l.strip()]
    at = lambda l: json.loads(l)["at"][11:]
    info = {"day": day, "weekday": wd, "scenario": sc.get("name", "an ordinary day"), "notes": []}
    handled = refused = 0
    timing.update(play_s=0.0, events=0)
    day_t0 = time.time()
    metrics(reset=True)

    # --- scenario edits to the day's events
    if sc.get("stockout"):
        ids = [json.loads(l)["id"] for l in ev if '"t":"order"' in l and "19:30" <= at(l) < "20:30" and '"channel":"whatsapp"' not in l][:6]
        keep = []
        for l in ev:
            j = json.loads(l)
            if j.get("id") in ids and j["t"] != "order":
                continue
            keep.append(l)
            if j["t"] == "order" and j["id"] in ids:
                keep.append(json.dumps({"t": "status", "id": j["id"], "status": "cancelled", "by": "restaurant", "at": j["at"]}))
        ev = keep
        info["notes"].append(f"chicken ran out at 19:30: the kitchen cancelled {len(ids)} platform orders")
    dup = [l for l in ev if '"t":"order"' in l and at(l) < "14:00"][:6] if sc.get("duplicates") else []

    # --- morning
    ctl(f"biz clock {day}T09:30")
    ask(day, "09:30", "issues", sc.get("novel", False))
    info["issues_morning"] = [l for l in mcp("biz_issues", {})[1].split("\n") if l and "nothing needs you" not in l]
    if sc.get("file_gst"):
        ans, _ = approve(day, "09:35", "file october gst", "biz_file_gst")
        if "prepared" not in ans:
            ans, _ = approve(day, "09:35", "file gst for 2026-10", "biz_file_gst")
        check(day, "October's return lodged once", "simulated portal" in ans, ans)
        again = ctl("chat file gst for 2026-10")
        if "approval required" in again:
            ctl("press " + again.split("(id ")[1].split(")")[0])
            again = ctl("chat file gst for 2026-10")
        check(day, "a second filing is refused", "already filed" in again, again)
        g = mcp("biz_gst", {"period": "2026-10"})[1]
        check(day, "no input tax credit at the 5% rate", "Input tax credit: none" in g and "Filed on" in g, g)
        info["gst_october"] = g
        info["notes"].append("1st of the month: the phone listed October's GST return as due; the owner filed it with the key")
    if sc.get("payout"):
        for pf, cut in sc["payout"]["cuts"].items():
            ctl(f"biz payout {pf} {sc['payout']['from']} {sc['payout']['to']} {cut}")
        info["notes"].append(f"platform payouts arrived for {sc['payout']['from']}..{sc['payout']['to']}")
    if sc.get("claims") or sc.get("payout"):
        ask(day, "09:40", "payouts", sc.get("novel", False))
        for pf in state()["platforms"]:
            if pf["claimable"] > 0:
                name = pf["name"].lower()
                ans, rec = approve(day, "09:42", rng.choice([f"claim it from {name}", f"raise the claim with {name}"]), "biz_claim")
                if "filed with" not in ans:
                    issues_found.append({"day": day, "what": f"claim phrase did not file: {ans[:140]}", "source": rec["source"]})
                    ans, _ = approve(day, "09:42", f"claim {pf['claimable'] / 100:g} from {name}", "biz_claim")
                check(day, f"exact shortfall claimed from {name}", "filed with" in ans, ans)
        check(day, "nothing left to claim", all(p["claimable"] == 0 for p in state()["platforms"]))
    if sc.get("purchases"):
        for text in ["bought gas cylinder from HP Gas Agency for 1850 gst 333", "bought tissue rolls from Coastal Packaging for 420 gst 76"]:
            ans, _ = say(day, "09:45", text, "biz_purchase")
            check(day, "purchase recorded by voice", "purchase recorded" in ans, ans)
    if sc.get("forget"):
        before = state()["privacy"]["customers"]
        orders_before = sum(h["orders"] for h in state()["history"])
        ans, _ = approve(day, "09:50", "forget customers older than 14 days", "biz_forget")
        after = state()["privacy"]
        check(day, "old customers erased, books intact", "erased" in ans and after["customers"] < before and sum(h["orders"] for h in state()["history"]) == orders_before, ans)
        info["notes"].append(f"retention: {ans.replace('done: ', '')}")

    # --- lunch
    h, r, _ = play(day, [l for l in ev if at(l) < "15:00"])
    handled, refused = handled + h, refused + r
    if dup:
        h, r, out = play(day, dup)
        check(day, "duplicate webhooks refused", h == 0 and r == len(dup), out)
        info["notes"].append(f"a platform re-sent {len(dup)} orders: all refused as already in the book")
    if sc.get("restart"):
        before = state()["totals"]
        stop()
        start()
        check(day, "the book survives a restart", state()["totals"] == before, before)
        info["notes"].append("the phone restarted at 15:05: the sealed book and the learned habits were still there")
    ctl(f"biz clock {day}T15:10")
    ask(day, "15:10", "dashboard", sc.get("novel", False))
    ask(day, "15:12", "orders", sc.get("novel", False))
    if sc.get("typed"):
        cases = [
            ("+91 90000 77001|Madhavi K|Flat 12, Lotus Residency|Siripuram|upi|2 chicken fried rice and one hot wings", "W-", "read and billed"),
            ("+91 90000 77002|Ramesh V|Plot 4, Hill View|MVP Colony|cod|1 bucket, 2x popcorn, veg noodles", "W-", "read and billed"),
            ("+91 90000 77003|Anil S|Door 9-1|Akkayyapalem|upi|2 fried rice", "unclear", "asked which fried rice"),
            ("+91 90000 77004|Sunita R|Plot 22|Gajuwaka|upi|1 chicken bucket", "we deliver up to", "refused: too far"),
            ("+91 90000 77005|Kishore B|Main road|Anakapalle|upi|1 egg noodles", "we do not know", "refused: unknown area"),
            ("+91 90000 77006|Deepa M|Flat 3, Sai Residency|Dwaraka Nagar|upi|1 mutton biryani", "not on the menu", "refused: not on the menu"),
        ]
        for line, want, what in cases:
            out = ctl("biz wa " + line)
            check(day, f"typed order: {what}", want in out and (want != "W-" or "bill" in out), out)
        info["notes"].append("six orders typed on WhatsApp: two read and billed, four answered with the reason")
    if sc.get("ask_gst"):
        ask(day, "15:15", "gst", sc.get("novel", False))

    # --- evening
    h, r, out = play(day, [l for l in ev if at(l) >= "15:00"])
    handled, refused = handled + h, refused + r
    if refused:
        info["notes"].append(f"{refused} events refused: {out.splitlines()[1].strip() if len(out.splitlines()) > 1 else ''}")
    if sc.get("client"):
        leaks = []
        for tool in ["biz_orders", "biz_dashboard", "biz_riders", "biz_privacy", "biz_plan"]:
            ok, text = mcp(tool, {})
            if any(x in text for x in ["90000", "Residency", "Flat "]):
                leaks.append(tool)
        check(day, "an MCP client sees no customer details", not leaks, leaks)
        ok, text = mcp("biz_pay_rider", {"rider": "R1", "amount": "5000"})
        check(day, "an MCP client cannot pay anyone", (not ok) and "approval required" in text, text)
        ctl("decline " + text.split("(id ")[1].split(")")[0])
        ok, text = mcp("biz_forget", {"days": "0"})
        check(day, "an MCP client cannot erase customers", (not ok) and "approval required" in text, text)
        ctl("decline " + text.split("(id ")[1].split(")")[0])
        info["notes"].append("a Claude client read the business over MCP (masked) and asked to pay ₹5,000 and to erase customers: both waited for the key and the owner declined")
    if sc.get("privacy"):
        ask(day, "22:40", "privacy", sc.get("novel", False))
    closing(day, sc.get("novel", False))

    # --- the day's numbers
    st, m = state(), metrics()
    info.update({
        "events": {"handled": handled, "refused": refused},
        "totals": st["totals"], "channels": st["channels"], "open": st["open"],
        "sent": st["sent_by_kind"],
        "sources": {k: v for k, v in m["chats"].items()},
        "ready": m["ready"], "retrains": m["retrains"], "retrain_avg_ms": m["retrain_avg_ms"],
        "confirmed": m["confirmed"], "episodes": m["episodes"],
        "issues_night": [l for l in mcp("biz_issues", {})[1].split("\n") if l and "nothing needs you" not in l],
        "event_ms": round(timing["play_s"] * 1000 / max(timing["events"], 1), 2), "day_seconds": round(time.time() - day_t0, 1),
        "plan": st["plan"], "riders": st["riders"], "platforms": st["platforms"], "gst": st["gst"], "privacy": st["privacy"],
    })
    direct = next((c["orders"] for c in st["channels"] if c["name"] == "whatsapp"), 0)
    check(day, "every direct order got exactly one bill", info["sent"].get("bill", 0) == direct, f"{info['sent']} vs {direct} direct orders")
    check(day, "no rider is tracked after the last drop", st["privacy"]["riders_tracked"] == 0 or bool(st["open"]), st["privacy"]["riders_tracked"])
    json.dump(st, open(os.path.join(OUT, "states", f"{day}.json"), "w"))
    reqs = [r for r in requests if r["day"] == day and r["expect"]]
    first = [r for r in reqs if r["note"] in ("", "after the key")]
    print(f"   {st['totals']['orders']} orders, sales ₹{st['totals']['sales'] / 100:,.0f}; {len(reqs)} requests, "
          f"{sum(r['ok'] for r in first)}/{len(first)} understood first time; {info['event_ms']} ms/event, day took {info['day_seconds']}s; sources {dict((k, v['n']) for k, v in m['chats'].items())}; ready {m['ready']}")
    return info


SCENARIOS = {
    0: {"name": "first day; a short payout from October is waiting", "claims": True},
    1: {"name": "Saturday rush", "scale": 1.15},
    2: {"name": "1st of the month: GST return due", "file_gst": True, "ask_gst": True},
    3: {"name": "rain: WhatsApp orders double", "scale": 1.25, "direct": 0.42},
    4: {"name": "customers type their orders", "typed": True},
    5: {"name": "platforms pay last week, Zomato short", "payout": {"from": "2026-10-26", "to": "2026-11-01", "cuts": {"swiggy": 0, "zomato": 650}}},
    6: {"name": "chicken runs out at dinner", "stockout": True},
    7: {"name": "a platform re-sends orders", "duplicates": True},
    8: {"name": "the phone restarts mid-day", "restart": True, "scale": 1.1},
    9: {"name": "a Claude client connects over MCP", "client": True, "privacy": True},
    10: {"name": "purchases by voice; mid-month GST check", "purchases": True, "ask_gst": True},
    11: {"name": "the owner shortens how long customers are kept", "forget": True, "privacy": True},
    12: {"name": "platforms pay last week, Swiggy short", "payout": {"from": "2026-11-02", "to": "2026-11-08", "cuts": {"swiggy": 300, "zomato": 0}}},
    13: {"name": "the owner words everything a new way", "novel": True, "scale": 0.85, "ask_gst": True, "privacy": True, "claims": True},
    14: {"name": "today", "ask_gst": True},
}

# ---------------------------------------------------------------- go
if os.path.exists(PHONE):
    shutil.rmtree(PHONE)
for d in (PHONE, EVENTS, os.path.join(OUT, "states")):
    os.makedirs(d, exist_ok=True)
open(os.path.join(OUT, "agentd.log"), "w").close()
cfg = f'''data_dir = "{PHONE}/data"
listen = "127.0.0.1:{PORT}"
control_socket = "{SOCK}"
phone = "sim"
kdf_mib = 8
idle_secs = 86400
business_profile = "{ROOT}/examples/cafe/profile.toml"
model_timeout_secs = 120
'''
if HAVE_MODEL:
    cfg += f'small_url = "{A.model}"\n'
open(CFG, "w").write(cfg)
TOKEN = subprocess.run([AGENTD, "init", "-c", CFG], capture_output=True, text=True).stdout.strip().splitlines()[-1]
start()
print(f"model: {A.model if HAVE_MODEL else 'none (only fixed phrases, shortcuts and the classifier)'}")

first = date.fromisoformat(A.first)
hist = []
for k in range(A.history, 0, -1):
    d = (first - timedelta(days=k)).isoformat()
    p = os.path.join(EVENTS, f"{d}.jsonl")
    ctl(f"biz gen {d} {A.seed} 1.0 0 {p}")
    hist.append(open(p).read())
open(os.path.join(EVENTS, "history.jsonl"), "w").write("".join(hist))
t0 = time.time()
print("past books:", ctl("biz import " + os.path.join(EVENTS, "history.jsonl")), f"({time.time() - t0:.1f}s)")
# October's weekly payouts, as the platforms made them (Zomato kept ₹480 it was not owed)
for a, b, zcut in [("2026-10-01", "2026-10-04", 0), ("2026-10-05", "2026-10-11", 0), ("2026-10-12", "2026-10-18", 480), ("2026-10-19", "2026-10-25", 0)]:
    ctl(f"biz payout swiggy {a} {b} 0")
    ctl(f"biz payout zomato {a} {b} {zcut}")

days = []
for i in range(A.days):
    days.append(run_day(i, (first + timedelta(days=i)).isoformat()))

# ---------------------------------------------------------------- after the cycle
final_day = days[-1]["day"]
hits = []
for dirpath, _, files in os.walk(PHONE):
    for f in files:
        if not os.path.isfile(os.path.join(dirpath, f)):
            continue
        b = open(os.path.join(dirpath, f), "rb").read()
        for needle in [b"Residency", b"90000 1", b"90000 7", b"Madhavi", b"Lotus"]:
            if needle in b:
                hits.append(f"{needle.decode()} in {os.path.relpath(os.path.join(dirpath, f), PHONE)}")
check(final_day, "no customer name, number or address on disk in the clear", not hits, hits[:5])
v = ctl("verify")
check(final_day, "the ledger verifies after 15 days", "ledger ok" in v and "Ok" in v, v)
learn = ctl("learn status")
m = metrics()
results = {
    "model": A.model if HAVE_MODEL else None, "first": A.first, "days": days, "requests": requests, "checks": checks,
    "corrections": corrections, "issues_found": issues_found, "learn_status": learn, "final_metrics": m,
    "book_bytes": os.path.getsize(os.path.join(PHONE, "data", "business", "book.sealed")),
}
json.dump(results, open(os.path.join(OUT, "results.json"), "w"), indent=1)
json.dump(state(), open(os.path.join(OUT, "state.json"), "w"))
sample = next((json.loads(l)["id"] for l in open(os.path.join(EVENTS, f"{final_day}.jsonl")) if '"channel":"whatsapp"' in l), None)
if sample:
    ctl(f"biz bill {sample} {os.path.join(OUT, 'sample-bill.pdf')}")
stop()
bad = [c for c in checks if not c["ok"]]
print(f"\n{len(checks) - len(bad)} of {len(checks)} checks passed; {len(corrections)} corrections; {len(issues_found)} problems found on the way")
for c in bad:
    print("  FAILED", c["day"], c["name"], "—", c["detail"][:160])
for x in issues_found:
    print("  PROBLEM", x["day"], x["what"])
