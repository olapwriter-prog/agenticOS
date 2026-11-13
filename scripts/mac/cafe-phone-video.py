#!/usr/bin/env python3
"""A day in the café owner's life with an agenticOS phone, recorded as a video of its screen.

    python3 scripts/mac/cafe-phone-video.py [--out target/cafe-video]

Only three things go into the phone, as in real life:
  - the world: orders and deliveries arriving from Swiggy, Zomato and WhatsApp (seeded events),
    and messages customers type on WhatsApp;
  - the owner's own words ("chat ..."), never a tool name;
  - the owner's taps on Approve or Decline for what is waiting on the screen.
Which tool answers a request, which fix a problem needs, and when to ask: the phone decides.
Each frame is the phone's real screen (ui/, rendered from the running daemon), with a caption
saying what came in and what the phone chose and why.
"""
import argparse, json, os, shutil, signal, socket, subprocess, sys, time, urllib.request
from datetime import date, timedelta
from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
ap = argparse.ArgumentParser()
ap.add_argument("--out", default=os.path.join(ROOT, "target", "cafe-video"))
ap.add_argument("--model", default="http://127.0.0.1:8091")
ap.add_argument("--day", default="2026-11-13")
ap.add_argument("--seed", type=int, default=7)
A = ap.parse_args()
OUT = os.path.abspath(A.out)
PHONE, FRAMES = os.path.join(OUT, "phone"), os.path.join(OUT, "frames")
AGENTD, UI = os.path.join(ROOT, "target/release/agentd"), os.path.join(ROOT, "ui/target/release/agentos-ui")
CFG, SOCK, PORT = os.path.join(PHONE, "agentd.toml"), os.path.join(PHONE, "c.sock"), 8774
os.environ["AGENTD_PASSPHRASE"] = "video"
DAY = A.day


def ctl(line):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(600)
    s.connect(SOCK)
    s.sendall((line.replace("\n", " ") + "\n").encode())
    buf = b""
    while not buf.endswith(b"\n"):
        c = s.recv(1 << 16)
        if not c:
            break
        buf += c
    s.close()
    return buf.decode().rstrip("\n").replace("\x01", "\n")


def model_up():
    try:
        return urllib.request.urlopen(A.model + "/health", timeout=3).status == 200
    except Exception:
        return False


# ------------------------------------------------------------------ a fresh phone with a month behind it
for d in (PHONE, FRAMES):
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
cfg = f'''data_dir = "{PHONE}/data"
listen = "127.0.0.1:{PORT}"
control_socket = "{SOCK}"
phone = "sim"
kdf_mib = 8
idle_secs = 86400
business_profile = "{ROOT}/examples/cafe/profile.toml"
model_timeout_secs = 120
'''
HAVE_MODEL = model_up()
if HAVE_MODEL:
    cfg += f'small_url = "{A.model}"\n'
open(CFG, "w").write(cfg)
subprocess.run([AGENTD, "init", "-c", CFG], capture_output=True)
proc = subprocess.Popen([AGENTD, "run", "-c", CFG], stdout=open(os.path.join(OUT, "agentd.log"), "w"), stderr=subprocess.STDOUT)
for _ in range(100):
    try:
        ctl("stage")
        break
    except OSError:
        time.sleep(0.1)
print("model:", A.model if HAVE_MODEL else "none")

first = date.fromisoformat(DAY)
hist = []
for k in range(29, 0, -1):
    d = (first - timedelta(days=k)).isoformat()
    p = os.path.join(OUT, f"hist-{d}.jsonl")
    ctl(f"biz gen {d} {A.seed} 1.0 0 {p}")
    hist.append(open(p).read())
    os.remove(p)
open(os.path.join(OUT, "history.jsonl"), "w").write("".join(hist))
print(ctl("biz import " + os.path.join(OUT, "history.jsonl")))
# the platforms' weekly payouts for October; Zomato kept ₹480 it was not owed in the last one
for a, b, cut in [("2026-10-15", "2026-10-21", 0), ("2026-10-22", "2026-10-28", 480), ("2026-10-29", "2026-11-04", 0)]:
    ctl(f"biz payout swiggy {a} {b} 0")
    ctl(f"biz payout zomato {a} {b} {cut}")
# the past days' riders were settled on those days: what the history left waiting is cleared
for line in ctl("pending").splitlines():
    if line.startswith("#") and "biz_pay_rider" in line:
        ctl("decline " + line.split()[0][1:])
day_file = os.path.join(OUT, f"{DAY}.jsonl")
ctl(f"biz gen {DAY} {A.seed} 1.0 0 {day_file}")
events = [l for l in open(day_file).read().splitlines() if l.strip()]
at = lambda l: json.loads(l)["at"][11:]


# ------------------------------------------------------------------ frames
W, H = 1920, 1080
FONT = "/System/Library/Fonts/SFNS.ttf"
f = lambda s: ImageFont.truetype(FONT, s)
BG, INK, MUTE, ACC, OK, BAD = "#0e0f12", "#f2f1ee", "#9b9a96", "#5aa2ff", "#4cc38a", "#ff7a6b"
said = []        # what the owner said and what came back, for the screen's chat
frames = []      # (file, seconds)
log = []         # every step, for the results file
SOURCE = {"intent": "a phrase the phone knows", "classifier": "its classifier (no language model)", "shortcut": "something you taught it",
          "model": "the language model on the phone", "feedback": "your feedback", "code-answer": "code over the phone's data"}


def wrap(d, text, font, width):
    out = []
    for para in text.split("\n"):
        line = ""
        for w in para.split():
            t = (line + " " + w).strip()
            if d.textlength(t, font=font) <= width:
                line = t
            else:
                out.append(line)
                line = w
        out.append(line)
    return out


def snap(clock, title, lines, secs=5, kind="world"):
    """Render the phone's screen now and caption it."""
    chat = os.path.join(OUT, "chat.json")
    json.dump(said[-2:], open(chat, "w"))
    shot = os.path.join(OUT, "screen.png")
    env = dict(os.environ, AGENTD_SOCK=SOCK, AGENTOS_UI_CHAT=chat)
    subprocess.run([UI, "--snap", shot, "--scene", "live"], env=env, capture_output=True)
    im = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(im)
    phone = Image.open(shot).convert("RGB")
    ph = 980
    pw = int(phone.width * ph / phone.height)
    phone = phone.resize((pw, ph), Image.LANCZOS)
    x0, y0 = 150, (H - ph) // 2
    d.rounded_rectangle([x0 - 14, y0 - 14, x0 + pw + 14, y0 + ph + 14], 46, fill="#2a2b30")
    mask = Image.new("L", (pw, ph), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, pw, ph], 36, fill=255)
    im.paste(phone, (x0, y0), mask)
    tx, tw = x0 + pw + 110, W - (x0 + pw + 110) - 110
    d.text((tx, 120), f"Friday 13 November 2026  ·  {clock}", font=f(30), fill=MUTE)
    tag = {"world": ("WHAT CAME IN", ACC), "owner": ("THE OWNER", OK), "phone": ("THE PHONE, ON ITS OWN", "#e0b252"), "end": ("", INK)}[kind]
    if tag[0]:
        d.text((tx, 172), tag[0], font=f(22), fill=tag[1])
    y = 210
    for ln in wrap(d, title, f(46), tw):
        d.text((tx, y), ln, font=f(46), fill=INK)
        y += 58
    y += 18
    for head, body, col in lines:
        if head:
            d.text((tx, y), head.upper(), font=f(20), fill=MUTE)
            y += 30
        for ln in wrap(d, body, f(28), tw)[:7]:
            d.text((tx, y), ln, font=f(28), fill=col or INK)
            y += 38
        y += 18
    d.text((tx, H - 70), "agenticOS on a virtual phone. The café, its customers and its orders are made up.", font=f(20), fill=MUTE)
    name = f"f{len(frames):03d}.png"
    im.save(os.path.join(FRAMES, name))
    frames.append((name, secs))


def owner_says(clock, words, why=""):
    """The owner speaks. The phone chooses the tool; the caption reports its choice."""
    ctl(f"biz clock {DAY}T{clock}")
    t0 = time.time()
    ans = ctl("chat " + words)
    wall = time.time() - t0
    m = json.loads(ctl("metrics"))["last"]
    said.append([words, ans])
    tool = m["tool"] or "nothing"
    src = m["source"].split("+")
    how = SOURCE.get(src[0], src[0]) + (", from an answer it had ready" if "ready" in src else "")
    took = f"{m['ms']:.0f} ms" if m["ms"] < 1000 else f"{m['ms'] / 1000:.1f} s"
    log.append({"clock": clock, "said": words, "tool": tool, "source": m["source"], "ms": m["ms"], "answer": ans[:200]})
    first = ans.replace("done: ", "", 1).replace("not done: ", "", 1).split("\n")
    body = "\n".join(first[:4])
    lines = [("The phone chose", f"{tool.replace('biz_', '')}  ·  by {how}  ·  {took}", ACC)]
    if "approval required" in ans:
        lines.append(("And", "It needs the approval key: it is waiting on the screen.", None))
    else:
        lines.append(("It answered", body, None))
    if why:
        lines.append(("", why, MUTE))
    snap(clock, f"“{words}”", lines, 6, "owner")
    return ans


def waiting():
    st = json.loads(ctl("state"))
    return [p for p in st["pending"]]


def tap(clock, p, approve=True):
    """The owner taps Approve (or Decline) on one item on the screen."""
    ctl(f"biz clock {DAY}T{clock}")
    out = ctl(("press " if approve else "decline ") + str(p["id"]))
    log.append({"clock": clock, "tap": "approve" if approve else "decline", "tool": p["tool"], "args": p["args"], "answer": out[:200]})
    what = out.split("; done: ", 1)[-1] if "; done: " in out else out
    snap(clock, f"Taps Approve: {p['tool'].replace('biz_', '').replace('_', ' ')}",
         [("What the phone did", what, None), ("", "One tap. The phone had worked out the amount from its books; nobody typed it.", MUTE)], 5, "owner")


def play_until(t):
    global events
    now = [l for l in events if at(l) < t]
    events = [l for l in events if at(l) >= t]
    p = os.path.join(OUT, "chunk.jsonl")
    open(p, "w").write("\n".join(now) + "\n")
    ctl("biz play " + p)
    orders = [json.loads(l) for l in now if '"t":"order"' in l]
    by = {}
    for o in orders:
        by[o["channel"]] = by.get(o["channel"], 0) + 1
    return orders, by


# ------------------------------------------------------------------ the day
snap("09:00", "The phone that runs the café",
     [("", "Vizag Wok & Crunch sells noodles, fried rice and fried chicken on Swiggy, Zomato and WhatsApp. "
           "The phone takes the orders, sends bills, tracks riders, settles money and files tax.", None),
      ("How to read this video", "On the left is the phone's real screen. On the right: what came in, what the owner said or tapped, "
                                 "and what the phone chose to do. The owner never names a tool or a step.", MUTE)], 7, "end")

ctl(f"biz clock {DAY}T09:25")
ctl("biz propose")
pend = waiting()
snap("09:25", f"Before the owner asks anything, {len(pend)} fixes are waiting",
     [("What it found in the books", "\n".join("• " + (p.get("why") or p["tool"]) for p in pend), None),
      ("What it did", "It worked out the fix for each, with the amount, and put them on the screen for the approval key. "
                      "Money and filings never go out without it.", None)], 8, "phone")
owner_says("09:27", "anything i should worry about")
for p in waiting():
    tap("09:28", p)

orders, by = play_until("13:45")
s = json.loads(ctl("biz state"))
snap("13:45", f"Lunch so far: {len(orders)} orders came in",
     [("From", ", ".join(f"{k.capitalize()} {v}" for k, v in sorted(by.items())), None),
      ("What the phone did with each, unasked", f"Booked it, sent every WhatsApp customer the bill as a PDF ({s['sent_by_kind'].get('bill', 0)} so far), "
       f"gave the rider the address, and sent each customer the rider's position while the food was on the way ({s['sent_by_kind'].get('rider location', 0)} so far).", None),
      ("", "These are the owner's standing rules, set once: each message can only go to the customer of that order.", MUTE)], 7, "phone")

ctl(f"biz clock {DAY}T13:50")
out = ctl("biz wa +91 90000 12345|Lakshmi P|Flat 204, Sea Breeze Residency|MVP Colony|upi|2 chicken fried rice and one hot wings")
snap("13:50", "A customer types on WhatsApp: “2 chicken fried rice and one hot wings”",
     [("The phone", "Read the message against the menu, booked the order and sent the bill:", None),
      ("", out.split(";")[0] + "\n" + (out.split(";")[1].strip() if ";" in out else ""), MUTE)], 6, "world")
# the kitchen and the rider's app report the typed order's progress, as for any other
typed = out.split()[0]
for ev in [{"t": "status", "id": typed, "status": "preparing", "at": f"{DAY}T13:08"}, {"t": "assign", "id": typed, "rider": "R2", "at": f"{DAY}T13:20"},
           {"t": "status", "id": typed, "status": "out", "at": f"{DAY}T13:22"}, {"t": "status", "id": typed, "status": "delivered", "at": f"{DAY}T13:44"}]:
    ev["at"] = ev["at"][:11] + {"13:08": "13:52", "13:20": "14:02", "13:22": "14:04", "13:44": "14:24"}[ev["at"][11:]]
    events.append(json.dumps(ev))
events.sort(key=at)
ctl(f"biz clock {DAY}T13:51")
out = ctl("biz wa +91 90000 22222|Anil S|Door 9-1|Akkayyapalem|upi|2 fried rice")
snap("13:51", "Another types: “2 fried rice”",
     [("The phone", "Did not guess. Several dishes are fried rice, so it asked the customer on WhatsApp:", None),
      ("", out.split("asked the customer: ")[-1], MUTE)], 6, "world")

play_until("15:10")
owner_says("15:10", "how did we do at lunch")
owner_says("15:12", "which orders are still going")

orders, by = play_until("21:30")
owner_says("21:30", "are the boys holding a lot of cash", "A wording it was never taught: it is matched by meaning, not by fixed phrase.")

last = max(at(l) for l in events) if events else "22:30"
play_until("24:00")
h, m = map(int, max(last, "22:36").split(":"))
m += 4
CLOSE = [f"{h + (m + k) // 60:02d}:{(m + k) % 60:02d}" for k in range(0, 12, 2)]
ctl(f"biz clock {DAY}T{CLOSE[0]}")
ctl("biz propose")
pend = [p for p in waiting() if p["tool"] == "biz_pay_rider"]
snap(CLOSE[0], "Closing: every order is done, so the riders' day is final",
     [("Waiting on the screen", "\n".join(f"• pay {p['args'].get('rider')} ₹{p['args'].get('amount')}: {p.get('why', '')}" for p in pend), None),
      ("What it did", "It proposed each rider's pay from the deliveries and kilometres in its books. The cash they collected from customers "
                      "it does not assume: only the owner knows the money is in his hand.", None)], 8, "phone")
riders = json.loads(ctl("biz state"))["riders"]
for r in riders:
    if r["cash_held"] > 0:
        owner_says(CLOSE[1], f"took the cash from {r['name'].lower()}")
for p in waiting():
    if p["tool"] == "biz_pay_rider":
        tap(CLOSE[2], p)
owner_says(CLOSE[3], "what should the kitchen get ready for tomorrow")
owner_says(CLOSE[4], "give me the day's summary")

# ------------------------------------------------------------------ the end
srcs = {}
for e in log:
    if "said" in e:
        k = e["source"].split("+")[0]
        srcs[k] = srcs.get(k, 0) + 1
st = json.loads(ctl("biz state"))
taps = sum(1 for e in log if "tap" in e)
snap(CLOSE[5], "What the owner did, and what the phone did",
     [("The owner", f"Said {sum(1 for e in log if 'said' in e)} things in his own words and tapped Approve {taps} times.", None),
      ("The phone chose", ", ".join(f"{n} by {SOURCE.get(k, k)}" for k, n in sorted(srcs.items(), key=lambda x: -x[1])), None),
      ("On its own", f"{st['totals']['orders']} orders booked, {st['sent_by_kind'].get('bill', 0)} bills and {st['sent_by_kind'].get('rider location', 0)} rider positions sent, "
                     "short payouts claimed from Swiggy and Zomato, last month's GST return filed and both riders paid, each after one tap.", None),
      ("Never", "No customer's name, number or address left the phone except on their own bill and to their own rider.", MUTE)], 9, "end")

proc.send_signal(signal.SIGTERM)
proc.wait(timeout=30)
open(os.path.join(FRAMES, "frames.txt"), "w").write("\n".join(f"{n} {s}" for n, s in frames) + "\n")
json.dump({"model": A.model if HAVE_MODEL else None, "steps": log}, open(os.path.join(OUT, "steps.json"), "w"), indent=1)
for e in log:
    print(e.get("clock"), e.get("said") or e.get("tap"), "->", e.get("tool"), e.get("source", ""), "|", (e.get("answer") or "")[:110].replace("\n", " / "))
print(f"{len(frames)} frames in {FRAMES}")
