"""Minimal W3C WebDriver client (stdlib only), enough to drive the Tauri app through WebKitWebDriver."""
import base64, json, time, urllib.request

EL = "element-6066-11e4-a52e-4f735466cecf"

class WD:
    def __init__(self, base="http://127.0.0.1:4444"):
        self.base, self.sid = base, None

    def _req(self, method, path, body=None):
        data = json.dumps(body).encode() if body is not None else (b"{}" if method == "POST" else None)
        r = urllib.request.Request(self.base + path, data=data, method=method, headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(r, timeout=150) as resp:
                return json.loads(resp.read() or b"{}").get("value")
        except urllib.error.HTTPError as e:
            raise RuntimeError(f"{method} {path}: {e.read().decode()[:300]}")

    def start(self, binary, args=()):
        caps = {"capabilities": {"alwaysMatch": {"webkitgtk:browserOptions": {"binary": binary, "args": list(args)}}}}
        self.sid = self._req("POST", "/session", caps)["sessionId"]

    def quit(self):
        if self.sid:
            try: self._req("DELETE", f"/session/{self.sid}")
            except Exception: pass

    def _s(self, p): return f"/session/{self.sid}{p}"
    def find_all(self, css): return [e[EL] for e in self._req("POST", self._s("/elements"), {"using": "css selector", "value": css})]
    def find(self, css):
        r = self.find_all(css); return r[0] if r else None
    def click(self, el): self._req("POST", self._s(f"/element/{el}/click"))
    def clear(self, el): self._req("POST", self._s(f"/element/{el}/clear"))
    def keys(self, el, text): self._req("POST", self._s(f"/element/{el}/value"), {"text": text})
    def text(self, el): return self.js("return arguments[0].innerText", {EL: el}) or ""
    def attr(self, el, name): return self._req("GET", self._s(f"/element/{el}/attribute/{name}"))
    def enabled(self, el): return self._req("GET", self._s(f"/element/{el}/enabled"))
    def js(self, script, *args): return self._req("POST", self._s("/execute/sync"), {"script": script, "args": list(args)})
    def js_async(self, script, *args):
        """Runs script with a trailing `done` callback (WebDriver "execute async")."""
        return self._req("POST", self._s("/execute/async"), {"script": script, "args": list(args)})

    def screenshot(self, path):
        open(path, "wb").write(base64.b64decode(self._req("GET", self._s("/screenshot"))))

    def wait(self, fn, timeout=20, what="condition"):
        end = time.time() + timeout
        last = None
        while time.time() < end:
            try:
                last = fn()
                if last: return last
            except RuntimeError as e:
                last = e
            time.sleep(0.25)
        raise TimeoutError(f"timed out waiting for {what} (last: {last})")

    def wait_css(self, css, timeout=20): return self.wait(lambda: self.find(css), timeout, css)
