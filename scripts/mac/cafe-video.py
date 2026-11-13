#!/usr/bin/env python3
"""Draw the 15-day run (target/cafe-sim/results.json) as frames, one per day, for cafe-video.swift.

    python3 scripts/mac/cafe-video.py [results.json] [frames-dir]
    swift scripts/mac/cafe-video.swift <frames-dir> <out.mp4>
"""
import json, os, sys, textwrap
from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
SRC = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "target/cafe-sim/results.json")
OUT = sys.argv[2] if len(sys.argv) > 2 else os.path.join(ROOT, "target/cafe-sim/frames")
R = json.load(open(SRC))
W, H = 1280, 720
FONT = "/System/Library/Fonts/SFNS.ttf"
f = lambda s: ImageFont.truetype(FONT, s)
BG, INK, MUTE, LINE, CARD = "#f7f6f2", "#1c1b19", "#6b6a66", "#dedcd5", "#ffffff"
CAT = [("fixed phrase", "#8a8f98"), ("learned shortcut", "#2f6fde"), ("classifier", "#12908a"), ("model", "#d9822b"), ("misunderstood", "#d43d3d")]
CH = {"swiggy": "#f08a24", "zomato": "#d6336c", "whatsapp": "#2f9e5b"}
rupees = lambda p: "₹{:,.0f}".format(p / 100)


def category(q):
    if not q["ok"]:
        return 4
    return {"intent": 0, "shortcut": 1, "classifier": 2, "model": 3}.get(q["source"].split("+")[0], 4)


def asked(day):
    return [q for q in R["requests"] if q["day"] == day and q["expect"] and q["note"] != "correction"]


def wrap(d, text, font, width):
    out, line = [], ""
    for w in text.split():
        t = (line + " " + w).strip()
        if d.textlength(t, font=font) <= width:
            line = t
        else:
            out.append(line)
            line = w
    return out + [line] if line else out


def card(d, x, y, w, h, title):
    d.rounded_rectangle([x, y, x + w, y + h], 10, fill=CARD, outline=LINE)
    d.text((x + 16, y + 12), title.upper(), font=f(13), fill=MUTE)


def frame(i, day):
    im = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(im)
    n = len(R["days"])
    d.text((40, 26), f"Day {i + 1} of {n}", font=f(30), fill=INK)
    d.text((40 + d.textlength(f"Day {i + 1} of {n}", font=f(30)) + 18, 36), f"{day['weekday']} {day['day']}", font=f(20), fill=MUTE)
    d.text((40, 70), day["scenario"][0].upper() + day["scenario"][1:], font=f(20), fill="#2f6fde")
    for k in range(n):  # progress
        d.rounded_rectangle([W - 40 - (n - k) * 22, 40, W - 40 - (n - k) * 22 + 16, 48], 3, fill=INK if k <= i else LINE)

    # --- the day
    x, y, w, h = 40, 112, 380, 372
    card(d, x, y, w, h, "the day's business")
    t = day["totals"]
    d.text((x + 16, y + 38), str(t["orders"]), font=f(40), fill=INK)
    d.text((x + 16 + d.textlength(str(t["orders"]), font=f(40)) + 8, y + 58), "orders", font=f(16), fill=MUTE)
    d.text((x + 190, y + 40), rupees(t["sales"]), font=f(24), fill=INK)
    d.text((x + 190, y + 70), f"sold · {rupees(t['net'])} reaches the owner", font=f(13), fill=MUTE)
    top = max(c["orders"] for c in day["channels"])
    for k, c in enumerate(sorted(day["channels"], key=lambda c: -c["orders"])):
        yy = y + 112 + k * 46
        d.text((x + 16, yy), {"whatsapp": "WhatsApp"}.get(c["name"], c["name"].capitalize()), font=f(15), fill=INK)
        d.rounded_rectangle([x + 100, yy + 2, x + 100 + int(190 * c["orders"] / top), yy + 18], 4, fill=CH.get(c["name"], MUTE))
        d.text((x + 100 + int(190 * c["orders"] / top) + 8, yy), str(c["orders"]), font=f(15), fill=INK)
        keep = c["net"] * 100 / c["sales"] if c["sales"] else 0
        d.text((x + 100, yy + 21), f"keeps ₹{keep:.0f} of every ₹100" + (f" · {c['cancelled']} cancelled" if c["cancelled"] else ""), font=f(12), fill=MUTE)
    s = day["sent"]
    rows = [(f"{s.get('bill', 0)} bills sent as PDF on WhatsApp", INK), (f"{s.get('rider location', 0)} rider positions sent to customers", INK),
            (f"{day['events']['handled']} events handled at {day['event_ms']} ms each" + (f", {day['events']['refused']} refused" if day["events"]["refused"] else ""), MUTE),
            (f"riders paid with the key: " + ", ".join(f"{r['name']} {rupees(r['paid'])}" for r in day["riders"] if r["paid"]), MUTE)]
    for k, (text, col) in enumerate(rows):
        d.text((x + 16, y + 262 + k * 25), text, font=f(14), fill=col)

    # --- how the phone understood the owner, building up day by day
    x, y, w, h = 440, 112, 400, 372
    card(d, x, y, w, h, "how the owner's words were understood")
    base, bw = y + h - 78, 18
    peak = max(len(asked(dd["day"])) for dd in R["days"])
    for k, dd in enumerate(R["days"][: i + 1]):
        counts = [0] * 5
        for q in asked(dd["day"]):
            counts[category(q)] += 1
        yy, bx = base, x + 24 + k * (bw + 6)
        for c, cnt in enumerate(counts):
            hh = int(cnt * 210 / peak)
            if hh:
                d.rectangle([bx, yy - hh, bx + bw, yy], fill=CAT[c][1])
                yy -= hh
        d.text((bx + bw / 2 - d.textlength(str(k + 1), font=f(11)) / 2, base + 4), str(k + 1), font=f(11), fill=INK if k == i else MUTE)
    d.line([x + 20, base, x + w - 20, base], fill=LINE)
    for c, (name, col) in enumerate(CAT):
        lx, ly = x + 20 + (c % 3) * 128, y + h - 46 + (c // 3) * 20
        d.rectangle([lx, ly + 3, lx + 10, ly + 13], fill=col)
        d.text((lx + 15, ly), name, font=f(12), fill=MUTE)
    today = asked(day["day"])
    right = sum(q["ok"] for q in today)
    d.text((x + 16, y + 36), f"{right} of {len(today)} right first time today", font=f(16), fill=INK)

    # --- what happened
    x, y, w, h = 860, 112, 380, 372
    card(d, x, y, w, h, "what the phone did about it")
    lines = []
    for note in day["notes"]:
        lines.append(("•", note, INK))
    for it in day["issues_morning"][:2]:
        lines.append(("!", "Told the owner at 9:30: " + it.split(": say")[0], "#b0531c"))
    for c in [c for c in R["corrections"] if c["day"] == day["day"]]:
        got = {"analyst": "wrote analysis code (45 s, wrong)", "q.strangers": "answered about text messages", "?": "another action"}.get(c["got"], c["got"].replace("biz_", "").replace("_", " "))
        lines.append(("✗", f"“{c['said']}” → {got}. Owner: “that was wrong”, said it plainly once; learned.", "#d43d3d"))
    for c in [c for c in R["checks"] if c["day"] == day["day"] and c["name"].startswith("exact shortfall claimed")]:
        lines.append(("•", c["detail"].replace("done: ", "").split(": ")[0].replace("claim ", "Claim ") + " (the exact shortfall, approved with the key)", INK))
    keyed = [q for q in R["requests"] if q["day"] == day["day"] and q["note"] == "after the key" and q["ok"]]
    if keyed:
        lines.append(("K", f"{len(keyed)} actions done only after the approval key (money, filings, erasing)", MUTE))
    if not day["notes"] and not day["issues_morning"]:
        lines.insert(0, ("•", "An ordinary day: orders billed, riders tracked and settled, tomorrow planned.", INK))
    yy = y + 40
    for mark, text, col in lines:
        d.text((x + 16, yy), mark, font=f(14), fill=col)
        for ln in wrap(d, text, f(14), w - 52):
            if yy > y + h - 28:
                break
            d.text((x + 36, yy), ln, font=f(14), fill=col)
            yy += 20
        yy += 8

    # --- counters, cumulative
    upto = R["days"][: i + 1]
    reads = sum(dd["ready"]["ahead_hits"] + dd["ready"]["misses"] for dd in upto)
    stats = [
        (str(day["confirmed"]), "requests confirmed by the owner (what it learns from)"),
        (str(sum(dd["retrains"] for dd in upto)), f"classifier retrains, {day['retrain_avg_ms']:.1f} ms each"),
        (f"{sum(dd['ready']['ahead_hits'] for dd in upto)} of {reads}", "reports ready before the owner asked"),
        (f"{sum(c['ok'] for c in R['checks'] if c['day'] <= day['day'])} of {sum(1 for c in R['checks'] if c['day'] <= day['day'])}", "business and privacy checks passed so far"),
    ]
    for k, (big, small) in enumerate(stats):
        x0 = 40 + k * 305
        d.rounded_rectangle([x0, 504, x0 + 285, 600], 10, fill=CARD, outline=LINE)
        d.text((x0 + 16, 516), big, font=f(30), fill=INK)
        for j, ln in enumerate(wrap(d, small, f(13), 255)):
            d.text((x0 + 16, 556 + j * 17), ln, font=f(13), fill=MUTE)
    plan = day["plan"]
    tip = plan["notes"][1] if len(plan["notes"]) > 1 else (plan["notes"][0] if plan["notes"] else "")
    d.text((40, 622), f"Tonight's plan for {plan['weekday']}: " + ", ".join(f"{it['name']} ×{it['forecast']}" for it in plan["items"][:3]) + f" · rush at {plan['peak'][0]['hour']}:00", font=f(15), fill=INK)
    for j, ln in enumerate(wrap(d, tip, f(13), W - 80)[:2]):
        d.text((40, 648 + j * 18), ln, font=f(13), fill=MUTE)
    d.text((W - 40 - d.textlength("agenticOS · simulated shop, invented data", font=f(12)), H - 24), "agenticOS · simulated shop, invented data", font=f(12), fill=MUTE)
    return im


def slide(title, sub, bullets):
    im = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(im)
    d.text((70, 70), title, font=f(40), fill=INK)
    yy = 130
    for ln in wrap(d, sub, f(20), W - 140):
        d.text((70, yy), ln, font=f(20), fill=MUTE)
        yy += 28
    yy += 22
    for head, body in bullets:
        d.text((70, yy), head, font=f(21), fill="#2f6fde")
        yy += 30
        for ln in wrap(d, body, f(17), W - 160):
            d.text((90, yy), ln, font=f(17), fill=INK)
            yy += 24
        yy += 12
    return im


os.makedirs(OUT, exist_ok=True)
for old in os.listdir(OUT):
    os.remove(os.path.join(OUT, old))
frames = []  # (image, seconds)
n_orders = sum(d["totals"]["orders"] for d in R["days"])
frames.append((slide(
    "A café owner's phone, fifteen days",
    "Vizag Wok & Crunch (made up) sells noodles, fried rice and fried chicken on Swiggy, Zomato and WhatsApp. One daemon on the phone takes the orders, bills, tracks, settles, files and plans. This is a recorded run on the virtual phone.",
    [("What is real here", f"The agenticOS daemon, its tiers and approval key, the sealed book, the learning and the cache are the real code. {n_orders:,} orders over 15 days, and a month of earlier books, come from a seeded generator."),
     ("What the owner does", "Talks to the phone three times a day in his own words, approves money with the key, and says “that was wrong” when it misunderstands."),
     ("What each day adds", "One thing going wrong: a short payout, the GST deadline, rain, typed orders, a stock-out, duplicate webhooks, a restart, a client over MCP.")]), 7))
for i, day in enumerate(R["days"]):
    frames.append((frame(i, day), 4))
asked_all = [q for q in R["requests"] if q["expect"] and q["note"] != "correction"]
med = lambda src: sorted(q["ms"] for q in R["requests"] if q["source"].split("+")[0] == src)[len([q for q in R["requests"] if q["source"].split("+")[0] == src]) // 2]
frames.append((slide(
    "What fifteen days showed",
    f"{sum(q['ok'] for q in asked_all)} of {len(asked_all)} requests were understood first time; {len(R['corrections'])} needed one correction and were right from then on. {sum(c['ok'] for c in R['checks'])} of {len(R['checks'])} business and privacy checks passed.",
    [("Learning", f"By day 2 most requests ran as learned shortcuts (median {med('shortcut'):.1f} ms). The model was needed {sum(1 for q in R['requests'] if q['source'].startswith('model'))} times (about 0.7 s each)."),
     ("Ready before asked", f"From day 3 the closing reports were prepared while the owner read the dashboard: {sum(d['ready']['ahead_hits'] for d in R['days'])} hits. That is {sum(d['ready']['ahead_hits'] for d in R['days']) * 100 // max(1, sum(d['ready']['ahead_hits'] + d['ready']['misses'] for d in R['days']))}% of report requests. It saves under a millisecond each on this machine, and {sum(d['ready']['ahead_hits'] for d in R['days']) * 100 // max(1, sum(d['ready']['prepared'] for d in R['days']))}% of what it prepares is used."),
     ("Found and fixed in testing", "A learned request replayed yesterday's amount; the word “it” reached the approval screen as an amount; business questions were sent to a code-writing model for 45 to 60 s; a correction landed on the wrong request; every order rewrote the whole book; reports slowed with the year's orders."),
     ("Privacy", "No customer name, number or address on disk in the clear or in any answer; bills and rider positions went only to the customer of that order; an MCP client could read masked figures and could not pay or erase.")]), 12))
lines = []
for k, (im, secs) in enumerate(frames):
    name = f"f{k:03d}.png"
    im.save(os.path.join(OUT, name))
    lines.append(f"{name} {secs}")
open(os.path.join(OUT, "frames.txt"), "w").write("\n".join(lines) + "\n")
print(f"{len(frames)} frames, {sum(s for _, s in frames)} s, in {OUT}")
