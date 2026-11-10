#!/usr/bin/env python3
"""A video of the OS as it is today, on the simulated phone (crates/phonesim).

  sudo python3 scripts/phonesim-demo.py record demo.jsonl   # run agentd on the simulator, log every beat
  sudo python3 scripts/phonesim-demo.py record-partner partner.jsonl   # the business partner, with the world
  python3 scripts/phonesim-demo.py render demo.jsonl demo.mp4   # draw the frames, make the video (ffmpeg)

Everything shown is real: the replies are agentd's over its control socket, the screen on the
right is drawn from agentd's own `state` JSON (what the Rune UI reads), the key presses are
input events on the simulated volume-down device, the texts and calls come from the simulated
network. Nothing is scripted on the agent's side.
"""
import json, os, shutil, subprocess, sys, time, textwrap

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
W = os.environ.get("PHONESIM_WORK", "/tmp/agentd-phonesim-demo")
MUM, RAVI = "+919876543210", "+919812345678"


# ---------------------------------------------------------------- record -------------------
def sh(*cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


class Phone:
    def __init__(self):
        self.agentd = f"{ROOT}/target/debug/agentd"
        self.sim = f"{ROOT}/target/debug/agentos-phonesim"
        self.conf = f"{W}/agentd.toml"
        self.log = []

    def start(self, partner=False):
        subprocess.run(["cargo", "build", "-q", "-p", "agentd", "-p", "agentos-phonesim"], cwd=ROOT, check=True)
        subprocess.run(["pkill", "-f", "agentos-phonesim keep-input"])
        shutil.rmtree(W, ignore_errors=True)
        os.makedirs(f"{W}/sim"), os.makedirs(f"{W}/data")
        os.environ["PHONESIM_DIR"] = f"{W}/sim"
        os.environ["PHONESIM_REQUIRE_SESSION"] = "1"
        os.environ["AGENTD_PASSPHRASE"] = "demo"
        for line in sh(self.sim, "init").stdout.splitlines():
            if line.startswith("export "):
                k, v = line[7:].split("=", 1)
                os.environ[k] = v.replace("$PATH", os.environ["PATH"])
        # the audio session, as its own user
        if sh("id", "agentd-session").returncode != 0:
            sh("useradd", "-r", "-M", "-d", "/nonexistent", "-s", "/usr/sbin/nologin", "agentd-session")
        su = sh("id", "-u", "agentd-session").stdout.strip()
        sg = sh("id", "-g", "agentd-session").stdout.strip()
        os.makedirs(f"/run/user/{su}", exist_ok=True)
        sh("chown", f"agentd-session:agentd-session", f"/run/user/{su}"), sh("chmod", "700", f"/run/user/{su}")
        self.session = subprocess.Popen(
            ["/usr/bin/setpriv", f"--reuid={su}", f"--regid={sg}", "--clear-groups", "env", "-i", "PATH=/usr/bin:/bin",
             "sh", f"{ROOT}/os/postmarketos/agentd-session.sh"], stdout=open(f"{W}/session.log", "w"), stderr=subprocess.STDOUT)
        open(self.conf, "w").write(f"""data_dir = "{W}/data"
listen = "127.0.0.1:18767"
kdf_mib = 1
idle_secs = 3600
phone = "modemmanager"
leds_dir = "{W}/sim/leds"
dbus_monitor = "/usr/bin/dbus-monitor"
session_user = "agentd-session"
approval_key = "{W}/sim/input-event0"
quiet_start_min = 0
quiet_end_min = 0
daily_budget = 1000
""")
        if partner:
            self.world = subprocess.Popen([self.sim, "world", "18900"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            os.environ["PHONESIM_WORLD"] = "http://127.0.0.1:18900"
            time.sleep(0.5)
            with open(self.conf, "a") as f:
                f.write("""
[partner]
world_url = "http://127.0.0.1:18900"
business_name = "Lakshmi Widgets"
gstin = "29AAACM1234A1Z4"
vpa = "lakshmiwidgets@upi"
city = "Bengaluru"
interests = ["music", "trekking"]
""")
        sh(self.agentd, "init", "-c", self.conf)
        self.proc = subprocess.Popen([self.agentd, "run", "-c", self.conf], stdout=open(f"{W}/agentd.log", "w"), stderr=subprocess.STDOUT)
        for _ in range(100):
            if sh(self.agentd, "ctl", "-c", self.conf, "status").returncode == 0:
                break
            time.sleep(0.2)
        time.sleep(3)

    def ctl(self, *args):
        return sh(self.agentd, "ctl", "-c", self.conf, *args).stdout.replace("\x01", "\n").rstrip()

    def state(self):
        try:
            return json.loads(self.ctl("state"))
        except json.JSONDecodeError:
            return {}

    def beat(self, caption, who="", text="", reply=None, key=None, hold=2.5, pause=2.5):
        """One moment of the film: what the caption says, who says what, the agent's reply."""
        if key:
            sh(self.sim, "key", str(int(hold * 1000)))
            time.sleep(0.6)
        self.log.append({"t": time.time(), "caption": caption, "who": who, "text": text, "reply": reply,
                         "key": key, "state": self.state(), "pause": pause})

    def say(self, caption, text, pause=3.0):
        reply = self.ctl("chat", text)
        self.beat(caption, "you", text, reply, pause=pause)
        return reply

    def command(self, caption, *args, pause=3.0):
        """A control command (agentd ctl), not a chat phrase."""
        reply = self.ctl(*args)
        self.beat(caption, "you", "ctl " + " ".join(args), reply, pause=pause)
        return reply

    def network(self, caption, *sim_args, wait=1.5, pause=3.0, announce=True):
        sh(self.sim, *sim_args)
        time.sleep(wait)
        if not announce:
            self.beat(caption, "network", " ".join(sim_args), None, pause=pause)
            return
        # what agentd announced for it, from its log
        said = [l.split(": ", 2)[-1] for l in open(f"{W}/agentd.log").read().splitlines() if "agentd: phone (" in l]
        self.beat(caption, "network", " ".join(sim_args[1:]), said[-1] if said else None, pause=pause)

    def approve(self, caption, text, pause=3.0):
        """A High action: asked, it waits; the key; asked again, done."""
        self.say(caption, text, pause=pause)
        self.beat("Held for two seconds: approved.", key="hold", hold=2.5, pause=2.0)
        return self.say("", text, pause=pause)

    def stop(self):
        if getattr(self, "world", None):
            self.world.terminate()
        self.proc.terminate(), self.proc.wait()
        self.session.terminate()
        sh(self.sim, "stop")


def record_partner(out):
    p = Phone()
    p.start(partner=True)
    p.beat("Agentic OS as a business partner. Every screen here is one agentd generated for the request. Customers, banks, the GST network and shops are simulated.", pause=5)
    p.say("Morning. One inbox for WhatsApp Business and email; a reply drafted for each customer.", "inbox", pause=5)
    p.say("", "add item Widget at 500 gst 18", pause=1.5)
    p.say("", "add item Repair at 1500 gst 18", pause=1.5)
    p.say("Customers are saved with their GSTIN, check digit verified.", "add customer Patil Stores gstin 27AAPFU0939F1ZV phone 98222 22222", pause=3)
    p.say("Patil Stores ordered on WhatsApp. The agent priced both items with GST.", "show chat with Patil Stores", pause=5)
    p.say("The invoice: Maharashtra is another state, so IGST. Numbered for the financial year.", "invoice Patil Stores for 10 x Widget, 2 x Repair", pause=5)
    num = [l for l in p.ctl("chat", "invoices").split() if l.startswith("INV/")][0]
    p.approve("Sending it is a message to a customer: it waits for the approval key.", f"send invoice {num}")
    p.network("The customer pays ₹9,440 by UPI, quoting the invoice.", "collect", "patil@okhdfc", "9440", num, announce=False)
    p.say("The payment is matched to the invoice by itself.", "who owes me", pause=4)
    p.say("Bills arrive by email; the agent reads amounts, due dates, GST and UPI addresses.", "what bills are due", pause=5)
    p.approve("Paying is money: the key again. The bill's GST becomes input tax credit.", "pay bill m-103")
    p.say("GST for the month: sales, input credit, and the tax left to pay after set-off.", "gst for this month", pause=6)
    p.approve("Filing GSTR-3B through the GST Suvidha Provider: the key, then an ARN.", "file gstr-3b for this month")
    p.say("Evening. What you buy often, from every platform's order history.", "what do i usually buy", pause=4)
    p.say("The best price for your usual atta across three platforms: one option, not a list.", "best price for atta", pause=5)
    p.approve("Bought from the wallet at the live price.", "buy best")
    p.say("News ranked by what you care about.", "news", pause=5)
    p.say("Things to do, picked by your interests.", "suggest adventures", pause=5)
    p.approve("Booking pays and issues tickets; booking a trek teaches the agent you like trekking.", "book EV-102 for 2")
    p.command("Every night on the charger, the model trains on what you confirmed that day, and keeps the change only if it does better.", "learn", "status", pause=5)
    p.beat("Real WhatsApp, GST filing, UPI and shops need their accounts: a URL and a key in agentd's vault. See docs/ROADMAP.md.", pause=5)
    p.stop()
    with open(out, "w") as f:
        for b in p.log:
            f.write(json.dumps(b) + "\n")
    print(f"{len(p.log)} beats -> {out}")


def record(out):
    p = Phone()
    p.start()
    p.beat("Agentic OS on a simulated phone. No screen is needed: agentd is the phone. Every reply here is the real daemon's.", pause=4)
    p.say("Low-risk things just happen.", "phone status")
    p.say("Contacts are kept by the agent.", "save Mum as +91 98765 43210")
    p.say("", "save Ravi as +91 98123 45678", pause=1.5)
    p.network("A text arrives. ModemManager's D-Bus signal wakes the agent at once; it alerts through feedbackd.", "sms", MUM, "dinner at 8?")
    p.say("", "read my messages")
    p.network("Someone rings. The agent announces who, by name.", "call", RAVI)
    p.say("Answering is medium risk: no key needed.", "answer")
    p.say("", "hang up", pause=2)
    p.say("Calling out is high risk: the agent refuses to act until the human presses the key.", "call mum")
    p.beat("The approval key is volume-down held for two seconds. A short tap does nothing.", key="tap", hold=0.3, pause=2.5)
    p.beat("Held for two seconds: approved. Exactly one action, exactly once.", key="hold", hold=2.5, pause=3)
    p.say("", "call mum")
    p.say("", "hang up", pause=2)
    p.say("Texts need the key too; the text goes to the modem through a file, never a command line.", "text ravi see you at 6")
    p.beat("", key="hold", pause=2)
    p.say("", "text ravi see you at 6")
    p.say("Questions about your own data are answered by code in the sandbox: the data never goes to a model.", "how many texts did Mum send me")
    p.say("The rest of the phone: torch, volume, Wi-Fi, data, balance, location.", "torch on", pause=2)
    p.say("", "volume 30", pause=2)
    p.say("", "scan wifi", pause=2.5)
    p.say("", "check balance *123#", pause=3)
    p.say("", "where am i", pause=2.5)
    p.command("Every action is in a hash-chained ledger, checked against a signed anchor.", "verify")
    p.command("On the charger, asleep, the small model trains on what you confirmed, and is kept only if it does better on held-out requests.", "learn", "status", pause=4.5)
    p.beat("What is not here yet: WhatsApp, invoicing, GST, payments, bookings, shopping. See docs/ROADMAP.md.", pause=5)
    p.stop()
    with open(out, "w") as f:
        for b in p.log:
            f.write(json.dumps(b) + "\n")
    print(f"{len(p.log)} beats -> {out}")


# ---------------------------------------------------------------- render -------------------
def render(log_path, out):
    from PIL import Image, ImageDraw, ImageFont
    beats = [json.loads(l) for l in open(log_path)]
    Wd, Hd, FPS = 1280, 720, 10
    font = lambda s, b=False: ImageFont.truetype(f"/usr/share/fonts/truetype/dejavu/DejaVuSans{'-Bold' if b else ''}.ttf", s)
    mono = lambda s: ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", s)
    BG, PANEL, INK, DIM, YOU, AGENT, NET, WARN, OK = "#0f1115", "#171a21", "#e8e8e8", "#8a909a", "#7cc4ff", "#c6f08a", "#f0b86a", "#ffd166", "#8be0a0"
    frames_dir = f"{W}/frames"
    shutil.rmtree(frames_dir, ignore_errors=True)
    os.makedirs(frames_dir)
    n = 0
    transcript = []
    for i, b in enumerate(beats):
        if b["who"]:
            transcript.append((b["who"], b["text"]))
        if b["reply"]:
            transcript.append(("agent", b["reply"]))
        transcript = transcript[-10:]
        st = b.get("state") or {}
        for f in range(int(b["pause"] * FPS)):
            im = Image.new("RGB", (Wd, Hd), BG)
            d = ImageDraw.Draw(im)
            d.text((28, 18), "Agentic OS · simulated phone · everything shown is the real daemon", font=font(16), fill=DIM)
            # transcript: the newest lines that fit, bottom-aligned to the latest
            d.rounded_rectangle((24, 50, 820, 600), 14, fill=PANEL)
            rows = []  # (label, label colour, text, text colour, gap after)
            for who, text in transcript:
                col = {"you": YOU, "agent": AGENT, "network": NET}[who]
                label = {"you": "you", "agent": "agentd", "network": "network"}[who]
                lines = []
                for para in text.splitlines()[:7]:
                    lines += textwrap.wrap(para, 78) or [""]
                lines = lines[:7]
                for j, line in enumerate(lines):
                    rows.append((label if j == 0 else "", col, line, INK if who == "agent" else col, j == len(lines) - 1))
            fit, used = [], 0
            for r in reversed(rows):
                h = 20 + (8 if r[4] else 0)
                if used + h > 520:
                    break
                fit.append(r)
                used += h
            y = 66
            for label, lc, line, tc, gap in reversed(fit):
                d.text((40, y), f"{label:>8}", font=mono(15), fill=lc)
                d.text((136, y), line, font=mono(15), fill=tc)
                y += 20 + (8 if gap else 0)
            # key indicator
            if b.get("key") and f < FPS * (0.5 if b["key"] == "tap" else 2.5):
                d.rounded_rectangle((24, 612, 820, 652), 10, fill="#2a2412")
                d.text((40, 622), ("volume-down: tap" if b["key"] == "tap" else "volume-down: holding…  " + "■" * min(10, int(f / FPS * 4))), font=font(17, True), fill=WARN)
            # the phone screen, from agentd's state JSON: the generated screen if there is one
            px, py, pw, ph = 860, 50, 390, 600
            if (st.get("screen") or {}).get("ui"):
                draw_screen(d, st["screen"]["ui"], px, py, pw, ph, font, b.get("key"))
                if b["caption"]:
                    d.rounded_rectangle((24, 660, 1256, 708), 12, fill="#1d2230")
                    cap = textwrap.wrap(b["caption"], 118)
                    d.text((40, 668 if len(cap) > 1 else 676), "\n".join(cap[:2]), font=font(16), fill=INK)
                im.save(f"{frames_dir}/{n:05d}.png")
                n += 1
                continue
            d.rounded_rectangle((px, py, px + pw, py + ph), 28, fill="#000000", outline="#333", width=3)
            d.rounded_rectangle((px + 12, py + 12, px + pw - 12, py + ph - 12), 20, fill=PANEL)
            d.text((px + 28, py + 26), f"stage {st.get('stage', '?')}", font=font(14), fill=DIM)
            d.text((px + 28, py + 48), textwrap.shorten(st.get("status", ""), 42), font=font(14, True), fill=INK)
            d.text((px + 28, py + 78), "\n".join(textwrap.wrap(st.get("phone", ""), 40)[:2]), font=font(13), fill=DIM)
            yy = py + 124
            pend = st.get("pending") or []
            d.text((px + 28, yy), "waiting for the key" if pend else "nothing waiting", font=font(13, True), fill=WARN if pend else OK)
            yy += 22
            for p_ in pend[:3]:
                args = " ".join(f"{k}={v}" for k, v in (p_.get("args") or {}).items())
                d.rounded_rectangle((px + 28, yy, px + pw - 28, yy + 40), 8, fill="#2a2412")
                d.text((px + 38, yy + 4), f"#{p_.get('id')} {p_.get('tool')}", font=font(13, True), fill=WARN)
                d.text((px + 38, yy + 22), textwrap.shorten(args, 40), font=font(12), fill=INK)
                yy += 48
            yy += 10
            d.text((px + 28, yy), "recent", font=font(13, True), fill=DIM)
            yy += 20
            for r in (st.get("recent") or [])[:8]:
                mark = {1: "✓", 0: "·"}.get(r.get("reward"), "?") if isinstance(r.get("reward"), (int, float)) else "·"
                d.text((px + 28, yy), textwrap.shorten(f"{r.get('tool','')}  {r.get('prompt','')}", 44), font=font(12), fill=INK)
                yy += 18
            # caption
            if b["caption"]:
                d.rounded_rectangle((24, 660, 1256, 708), 12, fill="#1d2230")
                cap = textwrap.wrap(b["caption"], 118)
                d.text((40, 668 if len(cap) > 1 else 676), "\n".join(cap[:2]), font=font(16), fill=INK)
            im.save(f"{frames_dir}/{n:05d}.png")
            n += 1
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-framerate", str(FPS), "-i", f"{frames_dir}/%05d.png",
                    "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "23", out], check=True)
    print(f"{n} frames, {n / FPS:.0f} s -> {out}")


def draw_screen(d, ui, px, py, pw, ph, font, key):
    """A UI document as a phone screen: title, cards (title, badge, lines, buttons)."""
    TONE = {"ok": "#8be0a0", "warn": "#ffd166", "info": "#7cc4ff"}
    d.rounded_rectangle((px, py, px + pw, py + ph), 28, fill="#000000", outline="#333", width=3)
    d.rounded_rectangle((px + 12, py + 12, px + pw - 12, py + ph - 12), 20, fill="#f6f7f9")
    x0, x1 = px + 24, px + pw - 24
    y = py + 26
    d.text((x0, y), textwrap.shorten(ui.get("title", ""), 34), font=font(18, True), fill="#111")
    y += 26
    if ui.get("subtitle"):
        d.text((x0, y), textwrap.shorten(ui["subtitle"], 46), font=font(12), fill="#666")
        y += 20
    y += 4
    for c in ui.get("cards", []):
        if y > py + ph - 70:
            break
        lines = [textwrap.shorten(l, 52) for l in c.get("lines", [])][:4]
        btns = c.get("actions", [])[:2]
        h = 30 + (16 if c.get("subtitle") else 0) + 16 * len(lines) + (30 if btns else 0)
        h = min(h, py + ph - 40 - y)
        d.rounded_rectangle((x0, y, x1, y + h), 12, fill="#ffffff", outline=TONE.get(c.get("tone", ""), "#dddddd"), width=2)
        d.text((x0 + 12, y + 8), textwrap.shorten(c.get("title", ""), 30 if c.get("badge") else 42), font=font(13, True), fill="#111")
        if c.get("badge"):
            bw = d.textlength(c["badge"], font=font(12, True)) + 14
            d.rounded_rectangle((x1 - bw - 8, y + 6, x1 - 8, y + 24), 9, fill="#eef3ff")
            d.text((x1 - bw - 1, y + 8), c["badge"], font=font(12, True), fill="#2a4cff")
        yy = y + 28
        if c.get("subtitle"):
            d.text((x0 + 12, yy), textwrap.shorten(c["subtitle"], 50), font=font(11), fill="#666")
            yy += 16
        for l in lines:
            if yy > y + h - 14:
                break
            d.text((x0 + 12, yy), l, font=font(11), fill="#333")
            yy += 16
        bx = x0 + 12
        for btn in btns:
            if yy + 24 > y + h:
                break
            label = textwrap.shorten(btn.get("label", ""), 20)
            bw = d.textlength(label, font=font(12, True)) + 20
            d.rounded_rectangle((bx, yy + 4, bx + bw, yy + 26), 11, fill="#2a4cff")
            d.text((bx + 10, yy + 7), label, font=font(12, True), fill="#ffffff")
            bx += bw + 8
        y += h + 8
    btns = ui.get("actions", [])[:2]
    if btns and y < py + ph - 60:
        bx = x0
        for btn in btns:
            label = textwrap.shorten(btn.get("label", ""), 24)
            bw = d.textlength(label, font=font(13, True)) + 24
            d.rounded_rectangle((bx, y + 4, bx + bw, y + 32), 14, fill="#111")
            d.text((bx + 12, y + 10), label, font=font(13, True), fill="#ffffff")
            bx += bw + 8
    if key:
        d.rounded_rectangle((px + 24, py + ph - 52, px + pw - 24, py + ph - 22), 10, fill="#2a2412")
        d.text((px + 36, py + ph - 46), "volume-down held: approve", font=font(13, True), fill="#ffd166")


if __name__ == "__main__":
    if sys.argv[1:2] == ["record"]:
        record(sys.argv[2])
    elif sys.argv[1:2] == ["record-partner"]:
        record_partner(sys.argv[2])
    elif sys.argv[1:2] == ["render"]:
        render(sys.argv[2], sys.argv[3])
    else:
        print(__doc__)
