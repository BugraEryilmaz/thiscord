#!/usr/bin/env python3
"""Browser check of the /app voice and video pipeline against a local backend
(scripts/web-call-check-backend.sh). Measurement tooling, not application code:
the page hooks below are injected through the DevTools protocol.

Two Chromium contexts with fake media devices join one voice channel. Page
hooks record: getUserMedia/clone tracks, which track each RTP sender carries,
send-gate (`track.enabled`) changes, microphone onsets from an independent
analyser, getStats calls, AudioContext lifetimes and track stops. Prints a JSON
report. Usage: python3 scripts/web-call-check.py voice|leak|camera  (needs chromium and
python3-websocket; prints a JSON report)
"""
import json, subprocess, sys, time, urllib.request, http.cookiejar, os, signal

BASE = "http://localhost:3900"
PORT = 9333
PASSWORD = "hill-climb-password-1"

HOOKS = r"""
(() => {
  const T = window.__hc = {gum: [], clones: 0, stops: 0, enabled: [], onsets: [], stats: [],
    replace: [], contexts: 0, closed: 0, start: performance.now()};
  const now = () => performance.now();
  const md = navigator.mediaDevices;
  if (md && md.getUserMedia) {
    const gum = md.getUserMedia.bind(md);
    md.getUserMedia = async (c) => {
      let s;
      try { s = await gum(c); } catch (e) { T.gum.push({t: now(), error: String(e), constraints: JSON.stringify(c)}); throw e; }
      T.gum.push({t: now(), audio: s.getAudioTracks().length, video: s.getVideoTracks().length});
      if (s.getAudioTracks().length && !T.probe) {
        // Independent onset probe on its own context: 256-sample window, 5 ms polls.
        const ctx = new AudioContext();
        const src = ctx.createMediaStreamSource(new MediaStream([s.getAudioTracks()[0].clone()]));
        const an = ctx.createAnalyser(); an.fftSize = 256; src.connect(an);
        const buf = new Float32Array(256); let loud = false;
        T.probe = setInterval(() => {
          an.getFloatTimeDomainData(buf);
          let p = 0; for (const v of buf) p = Math.max(p, Math.abs(v));
          if (!loud && p > 0.05) { loud = true; T.onsets.push(now()); }
          if (loud && p < 0.01) loud = false;
        }, 5);
        T.probeContext = ctx;
      }
      return s;
    };
  }
  T.calls = [];
  if (md && md.enumerateDevices) { const ed = md.enumerateDevices.bind(md); md.enumerateDevices = async () => { T.calls.push('enumerate'); return ed(); }; }
  document.addEventListener('click', e => T.calls.push('click:' + (e.target.closest('button')?.getAttribute('aria-label') || e.target.tagName)), true);
  window.addEventListener('unhandledrejection', e => T.calls.push('rejection:' + String(e.reason).slice(0, 120)));
  window.addEventListener('error', e => T.calls.push('error:' + String(e.message).slice(0, 120)));
  const P = MediaStreamTrack.prototype;
  const clone = P.clone; P.clone = function () { T.clones++; return clone.call(this); };
  const stop = P.stop; P.stop = function () { T.stops++; return stop.call(this); };
  const d = Object.getOwnPropertyDescriptor(P, 'enabled');
  Object.defineProperty(P, 'enabled', {get() { return d.get.call(this); },
    set(v) { if (this.kind === 'audio' && d.get.call(this) !== v) T.enabled.push({t: now(), v, label: this.label, id: this.id}); d.set.call(this, v); }});
  const AC = window.AudioContext;
  window.AudioContext = function (...a) { T.contexts++; const c = new AC(...a); const close = c.close.bind(c); c.close = () => { T.closed++; return close(); }; return c; };
  window.AudioContext.prototype = AC.prototype;
  const gs = RTCPeerConnection.prototype.getStats;
  RTCPeerConnection.prototype.getStats = function (...a) { T.stats.push(now()); return gs.apply(this, a); };
  T.views = []; T.states = [];
  const send = WebSocket.prototype.send;
  WebSocket.prototype.send = function (data) {
    if (typeof data === 'string') {
      if (data.includes('"screen_views"')) T.views.push({t: now(), n: (data.match(/"slot"/g) || []).length});
      if (data.includes('"type":"state"')) T.states.push(now());
      if (data.includes('"screen_keyframe"')) (T.keyframes = T.keyframes || []).push(now());
    }
    return send.call(this, data);
  };
  const PC = window.RTCPeerConnection; window.__hcPcs = [];
  window.RTCPeerConnection = function (...a) { const pc = new PC(...a); window.__hcPcs.push(pc); return pc; };
  window.RTCPeerConnection.prototype = PC.prototype;
  const rt = RTCRtpSender.prototype.replaceTrack;
  RTCRtpSender.prototype.replaceTrack = function (track) {
    T.replace.push({t: now(), kind: track && track.kind, label: track && track.label, id: track && track.id});
    return rt.call(this, track);
  };
})();
"""


def api(path, body, token=None):
    req = urllib.request.Request(BASE + path, data=json.dumps(body).encode(),
                                 headers={"content-type": "application/json", "origin": BASE})
    if token:
        req.add_header("authorization", "Bearer " + token)
    with urllib.request.urlopen(req) as r:
        return json.loads(r.read() or b"null")


class Cdp:
    def __init__(self, url):
        import websocket
        self.ws = websocket.create_connection(url, suppress_origin=True)
        self.n = 0
        self.events = []

    def call(self, method, params=None, session=None, timeout=60):
        self.n += 1
        msg = {"id": self.n, "method": method, "params": params or {}}
        if session:
            msg["sessionId"] = session
        self.ws.send(json.dumps(msg))
        end = time.time() + timeout
        while time.time() < end:
            m = json.loads(self.ws.recv())
            if m.get("method") in ("Runtime.exceptionThrown", "Runtime.consoleAPICalled", "Log.entryAdded"):
                self.events.append(json.dumps(m.get("params"))[:400])
            if m.get("id") == self.n:
                if "error" in m:
                    raise RuntimeError(f"{method}: {m['error']}")
                return m.get("result", {})
        raise TimeoutError(method)


def page(cdp, context):
    target = cdp.call("Target.createTarget", {"url": "about:blank", "browserContextId": context})["targetId"]
    session = cdp.call("Target.attachToTarget", {"targetId": target, "flatten": True})["sessionId"]
    cdp.call("Page.enable", session=session)
    cdp.call("Runtime.enable", session=session)
    cdp.call("Log.enable", session=session)
    cdp.call("Emulation.setDeviceMetricsOverride", {"width": 1600, "height": 1000, "deviceScaleFactor": 1, "mobile": False}, session=session)
    cdp.call("Page.addScriptToEvaluateOnNewDocument", {"source": HOOKS}, session=session)
    return session


def js(cdp, session, expr, timeout=60):
    r = cdp.call("Runtime.evaluate", {"expression": expr, "awaitPromise": True, "returnByValue": True,
                                      "userGesture": True}, session=session, timeout=timeout)
    if "exceptionDetails" in r:
        raise RuntimeError(json.dumps(r["exceptionDetails"])[:500])
    return r["result"].get("value")


def wait(cdp, session, expr, timeout=30):
    end = time.time() + timeout
    while time.time() < end:
        if js(cdp, session, expr):
            return True
        time.sleep(0.25)
    page = js(cdp, session, "location.href + ' | ' + document.body.innerText.slice(0, 800)")
    raise TimeoutError(expr + "\n" + str(page) + "\n" + "\n".join(cdp.events[-15:]))


def login(cdp, session, user):
    cdp.call("Page.navigate", {"url": BASE + "/app/"}, session=session)
    time.sleep(2)
    js(cdp, session, f"""fetch('/api/v1/account', {{method: 'POST', headers: {{'content-type': 'application/json',
        'x-thiscord-session': 'cookie'}}, body: JSON.stringify({{action: 'login', login: '{user}',
        password: '{PASSWORD}', device: 'smoke'}})}}).then(r => r.status)""")
    cdp.call("Page.reload", session=session)
    time.sleep(3)


def setup():
    tokens = {}
    for user in ["hcalice", "hcbob"]:
        try:
            r = api("/api/v1/account", {"action": "register", "username": user, "email": f"{user}@example.invalid",
                                        "password": PASSWORD, "device": "smoke"})
        except urllib.error.HTTPError:
            r = api("/api/v1/account", {"action": "login", "login": user, "password": PASSWORD, "device": "smoke"})
        tokens[user] = r["session"]["token"]
    # Verify emails through the development mail files, then make alice the
    # instance owner with the local CLI (idempotent failure once owned).
    import glob, re
    for token in tokens.values():
        api("/api/v1/account", {"action": "send_verification"}, token)
    deadline = time.time() + 20
    while time.time() < deadline:
        texts = [open(p).read() for p in glob.glob("/tmp/hc-mail/*.txt")]
        if all(any(f"To: {u}@" in t for t in texts) for u in tokens) or not texts and time.time() > deadline - 15:
            break
        time.sleep(0.5)
    for path in glob.glob("/tmp/hc-mail/*.txt"):
        text = open(path).read()
        link = re.search(r"(http://localhost:3900/api/v1/account/verify-email\?token=\S+)", text)
        if link:
            try:
                urllib.request.urlopen(link.group(1)).read()
            except Exception:
                pass
            os.remove(path)
    here = os.path.dirname(os.path.abspath(__file__))
    subprocess.run(["bash", "-l", os.path.join(here, "web-call-check-backend.sh"), "--bootstrap-owner", "hcalice"],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    a = tokens["hcalice"]
    P = "/api/v1/permissions"
    guilds = api(P, {"action": "list_guilds"}, a)["guilds"]
    existing = [g for g in guilds if g["name"] == "Hill climb"]
    guild = existing[0]["id"] if existing else None
    if guild is None:
        api(P, {"action": "create_guild", "name": "Hill climb"}, a)
        guild = [g for g in api(P, {"action": "list_guilds"}, a)["guilds"] if g["name"] == "Hill climb"][0]["id"]
    home = api(P, {"action": "view_guild", "guild_id": guild}, a)["home"]
    voice = [c for c in home["channels"] if c.get("kind") == "voice"]
    if not voice:
        api(P, {"action": "change", "guild_id": guild, "revision": home["guild"]["revision"],
                "change": {"action": "create_channel", "name": "Voice", "kind": "voice"}}, a)
        home = api(P, {"action": "view_guild", "guild_id": guild}, a)["home"]
        voice = [c for c in home["channels"] if c.get("kind") == "voice"]
    try:
        api(P, {"action": "change", "guild_id": guild, "revision": home["guild"]["revision"],
                "change": {"action": "add_member", "username": "hcbob"}}, a)
    except urllib.error.HTTPError:
        pass  # already a member
    return guild, voice[0]["id"]


def join(cdp, session, guild, channel, navigate=True):
    if navigate:
        cdp.call("Page.navigate", {"url": f"{BASE}/app/"}, session=session)
        time.sleep(3)
    # Open the server, then click its voice channel (a user gesture).
    js(cdp, session, f"document.querySelector('[data-context-server=\"{guild}\"]')?.click(), true")
    wait(cdp, session, f"!!document.querySelector('button.channel-item[data-context-channel=\"{channel}\"]')")
    js(cdp, session, f"document.querySelector('button.channel-item[data-context-channel=\"{channel}\"]').click(), true")
    wait(cdp, session, "!!document.querySelector('[aria-label=\"Disconnect voice\"]')", 30)


def report(cdp, session):
    return js(cdp, session, """(() => { const T = window.__hc; const s = T.stats;
      return {elapsed: performance.now() - T.start, gum: T.gum, clones: T.clones, stops: T.stops,
        contexts: T.contexts, closed: T.closed, replace: T.replace, enabled: T.enabled.slice(-40),
        onsets: T.onsets.slice(-40), stats_calls: s.length,
        stats_intervals: s.slice(1).map((t, i) => Math.round(t - s[i])).slice(-20)}; })()""")


def bursts(path="/tmp/hc-bursts.wav"):
    """Fake microphone input: 120 ms 440 Hz bursts every 1.5 s, so the 500 ms
    voice-activation hold closes and reopens on every burst."""
    if os.path.exists(path):
        return
    import math, struct, wave
    rate = 48000
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(b"".join(
            struct.pack("<h", int(12000 * math.sin(2 * math.pi * 440 * i / rate)) if (i / rate) % 1.5 < 0.12 else 0)
            for i in range(rate * 60)))


def main():
    bursts()
    scenario = sys.argv[1] if len(sys.argv) > 1 else "voice"
    guild, channel = setup()
    chrome = subprocess.Popen(["chromium", "--headless=new", f"--remote-debugging-port={PORT}",
                               "--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream",
                               *(["--use-file-for-fake-audio-capture=/tmp/hc-bursts.wav"] if os.path.exists("/tmp/hc-bursts.wav") else []),
                               "--autoplay-policy=no-user-gesture-required", "--no-first-run",
                               "--user-data-dir=/tmp/hc-chrome", "--no-sandbox", "about:blank"],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(50):
            try:
                ws = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{PORT}/json/version").read())["webSocketDebuggerUrl"]
                break
            except Exception:
                time.sleep(0.2)
        cdp = Cdp(ws)
        sessions = {}
        for user in ["hcalice", "hcbob"]:
            ctx = cdp.call("Target.createBrowserContext")["browserContextId"]
            sessions[user] = page(cdp, ctx)
            login(cdp, sessions[user], user)
        out = {"scenario": scenario}
        if scenario == "voice":
            for user in ["hcalice", "hcbob"]:
                join(cdp, sessions[user], guild, channel)
            time.sleep(30)
            for user, s in sessions.items():
                out[user] = report(cdp, s)
        elif scenario == "leak":
            s = sessions["hcalice"]
            join(cdp, sessions["hcbob"], guild, channel)
            for i in range(3):
                join(cdp, s, guild, channel, navigate=(i == 0))
                time.sleep(5)
                js(cdp, s, "document.querySelector('[aria-label=\"Disconnect voice\"]').click(), true")
                wait(cdp, s, "!document.querySelector('[aria-label=\"Disconnect voice\"]')", 15)
                time.sleep(2)
            out["hcalice"] = report(cdp, s)
        elif scenario == "camera":
            for user in ["hcalice", "hcbob"]:
                join(cdp, sessions[user], guild, channel)
            bob, alice = sessions["hcbob"], sessions["hcalice"]
            cdp.events.clear()
            wait(cdp, bob, "[...document.querySelectorAll('button[aria-label=\"Camera\"]')].some(b => b.offsetParent !== null && !b.disabled)", 30)
            js(cdp, bob, "[...document.querySelectorAll('button[aria-label=\"Camera\"]')].find(b => b.offsetParent !== null).click(), true")
            time.sleep(1)
            time.sleep(4)
            wait(cdp, alice, "!!document.querySelector('button.voice-person.has-stream')", 30)
            js(cdp, alice, "document.querySelector('button.voice-person.has-stream').click(), true")
            wait(cdp, alice, "[...document.querySelectorAll('video')].some(v => v.readyState >= 2 && v.videoWidth > 0)", 30)
            time.sleep(15)
            # Scroll the camera tile out of view (CSSOM transform), then back.
            marks = {"hidden": js(cdp, alice, "(document.querySelector('.stream-player video').closest('[data-media-key]') || document.querySelector('.stream-player')).style.transform = 'translateX(-10000px)', performance.now()")}
            time.sleep(4)
            marks["shown"] = js(cdp, alice, "(document.querySelector('.stream-player video').closest('[data-media-key]') || document.querySelector('.stream-player')).style.transform = '', performance.now()")
            time.sleep(4)
            out["hcalice"] = report(cdp, alice)
            out["hcalice"]["marks"] = marks
            out["hcalice"]["keyframes"] = js(cdp, alice, "window.__hc.keyframes || []")
            out["hcalice"]["views"] = js(cdp, alice, "window.__hc.views")
            out["hcalice"]["states"] = js(cdp, alice, "window.__hc.states")
            out["hcalice"]["video"] = js(cdp, alice, """(async () => { const out = [];
              for (const pc of window.__hcPcs) { const r = await pc.getStats(); r.forEach(s => {
                if (s.type === 'inbound-rtp' && s.kind === 'video' && s.framesDecoded) out.push({framesDecoded: s.framesDecoded, frameWidth: s.frameWidth, fps: s.framesPerSecond}); }); }
              return out; })()""")
            out["hcalice"]["tile_status"] = js(cdp, alice, "[...document.querySelectorAll('.stream-player-status')].map(e => e.innerText)")
            out["hcalice"]["players"] = js(cdp, alice, "[...document.querySelectorAll('video')].map(v => [v.videoWidth, v.videoHeight, v.readyState])")
        print(json.dumps(out, indent=1))
    finally:
        chrome.send_signal(signal.SIGTERM)
        chrome.wait(10)


if __name__ == "__main__":
    main()
