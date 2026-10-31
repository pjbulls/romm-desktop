import json, re, sys, hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

def romm_hash(blob):
    """RomM's Save.content_hash: md5 of a plain file, or of a zip's sorted '<entry>:<entry md5>' lines.
    (The mock used to md5 the raw bytes, which hid exactly the mismatch Argosy-compatible zips hit.)"""
    import io, zipfile
    try:
        z = zipfile.ZipFile(io.BytesIO(blob))
    except zipfile.BadZipFile:
        return hashlib.md5(blob).hexdigest()
    lines = [f"{n}:{hashlib.md5(z.read(n)).hexdigest()}" for n in sorted(z.namelist()) if not n.endswith('/')]
    return hashlib.md5("\n".join(lines).encode()).hexdigest()

STATES = {}  # id -> dict(rom_id, emulator, name, data, updated)
BASE = {}    # (device, rom, emu) -> hash this device last synced
SAVES = {}   # id -> dict(rom_id, emulator, slot, name, data, hash)
NEXT = [100]
LOG = open('/tmp/smoke/server.log', 'w')
SCOPES = ["me.read","platforms.read","roms.read","firmware.read","assets.read","assets.write","devices.read","devices.write"]
BIOS = b"FAKE-PS1-BIOS"
import os, struct

def iso9660(files):
    """A minimal valid ISO 9660 image with `files` in the root (what identifying a disc reads)."""
    S = 2048
    def both32(v): return struct.pack("<I", v) + struct.pack(">I", v)
    def both16(v): return struct.pack("<H", v) + struct.pack(">H", v)
    def rec(extent, size, is_dir, name):
        n = 33 + len(name); n += n % 2
        r = bytearray(n); r[0] = n; r[2:10] = both32(extent); r[10:18] = both32(size)
        r[18:25] = bytes([126, 1, 1, 0, 0, 0, 0]); r[25] = 2 if is_dir else 0; r[28:32] = both16(1)
        r[32] = len(name); r[33:33 + len(name)] = name
        return bytes(r)
    d = rec(18, S, True, b"\0") + rec(18, S, True, b"\1"); nxt = 19; placed = []
    for name, data in files:
        d += rec(nxt, len(data), False, name.encode() + b";1"); placed.append((nxt, data)); nxt += max(1, -(-len(data) // S))
    img = bytearray(nxt * S)
    p = 16 * S; img[p] = 1; img[p+1:p+6] = b"CD001"; img[p+6] = 1; img[p+8:p+72] = b" " * 64
    img[p+80:p+88] = both32(nxt); img[p+120:p+124] = both16(1); img[p+124:p+128] = both16(1); img[p+128:p+132] = both16(S)
    img[p+132:p+140] = both32(10); img[p+156:p+190] = rec(18, S, True, b"\0")[:34]; img[p+881] = 1
    t = 17 * S; img[t] = 0xFF; img[t+1:t+6] = b"CD001"; img[t+6] = 1
    img[18*S:18*S+len(d)] = d
    for e, data in placed: img[e*S:e*S+len(data)] = data
    return bytes(img)

GAME = os.environ.get("SMOKE_GAME", "ngc")
ROM_BYTES = b"FAKE-GC-ISO"
ROM = {"id": 7, "name": "Super Smash Bros. Melee", "fs_name": "Melee.iso", "fs_name_no_ext": "Melee", "fs_size_bytes": 11,
       "platform_id": 1, "platform_slug": "ngc", "platform_fs_slug": "ngc", "has_multiple_files": False, "files": []}
PLATS = [
  {"id": 1, "slug": "ngc", "fs_slug": "ngc", "name": "GameCube", "rom_count": 1, "firmware": []},
  {"id": 2, "slug": "psx", "fs_slug": "psx", "name": "PlayStation", "rom_count": 0,
   "firmware": [{"id": 9, "platform_id": 2, "file_name": "scph5501.bin", "file_size_bytes": len(BIOS),
                 "md5_hash": hashlib.md5(BIOS).hexdigest(), "is_verified": True, "missing_from_fs": False}]},
]

if GAME == "psp":
    # A real PSP disc image, named without its id: the game must be identified from inside it.
    ROM_BYTES = iso9660([("UMD_DATA.BIN", b"ULUS-10064|0000000000000000|0001|G")])
    ROM = {"id": 7, "name": "Crisis Core", "fs_name": "Crisis Core.iso", "fs_name_no_ext": "Crisis Core", "fs_size_bytes": len(ROM_BYTES),
           "platform_id": 3, "platform_slug": "psp", "platform_fs_slug": "psp", "has_multiple_files": False, "files": []}
    PLATS.append({"id": 3, "slug": "psp", "fs_slug": "psp", "name": "PSP", "rom_count": 1, "firmware": []})

def multipart_file(body, ctype, field='saveFile'):
    boundary = re.search(r'boundary=(.+)', ctype).group(1).encode()
    for part in body.split(b'--' + boundary):
        if (b'name="' + field.encode() + b'"') in part:
            head, _, data = part.partition(b'\r\n\r\n')
            return re.search(rb'filename="([^"]*)"', head).group(1).decode(), data.rsplit(b'\r\n', 1)[0]
    return None, None

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def send(self, code, obj=None, raw=None):
        body = raw if raw is not None else json.dumps(obj if obj is not None else {}).encode()
        self.send_response(code); self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)
    def authed(self):
        if self.headers.get('Authorization') != 'Bearer rmm_smoketoken':
            self.send(401, {"detail": "Not authenticated"}); return False
        return True
    def body(self):
        n = int(self.headers.get('Content-Length') or 0); return self.rfile.read(n)
    def do_GET(self):
        u = urlparse(self.path); p = u.path
        LOG.write(f"GET {self.path}\n"); LOG.flush()
        if not self.authed(): return
        if p == '/api/platforms': return self.send(200, PLATS)
        if p == '/api/roms': return self.send(200, {"items": [ROM], "total": 1, "limit": 50, "offset": 0})
        if p == '/api/roms/7': return self.send(200, ROM)
        if p == '/api/roms/7/content/' + ROM["fs_name"].replace(" ", "%20") or p == '/api/roms/7/content/' + ROM["fs_name"]: return self.send(200, raw=ROM_BYTES)
        if p == '/api/firmware/9/content/scph5501.bin': return self.send(200, raw=BIOS)
        if p == '/api/states':
            return self.send(200, [dict(id=i, rom_id=v['rom_id'], file_name=v['name'], file_size_bytes=len(v['data']), emulator=v['emulator'], updated_at=v['updated'], missing_from_fs=False) for i, v in STATES.items()])
        m = re.match(r'/api/states/(\d+)/content', p)
        if m: return self.send(200, raw=STATES[int(m.group(1))]['data'])
        m = re.match(r'/api/saves/(\d+)/content', p)
        if m:
            sv = SAVES[int(m.group(1))]; q = {k: v[0] for k, v in parse_qs(u.query).items()}
            if q.get('device_id'): BASE[(q['device_id'], sv['rom_id'], sv['emulator'])] = sv['hash']
            return self.send(200, raw=sv['data'])
        self.send(404, {"detail": "nope"})
    def do_PUT(self):
        u = urlparse(self.path); p = u.path; data = self.body()
        m = re.match(r'/api/states/(\d+)$', p)
        if not self.authed(): return
        if m:
            i = int(m.group(1)); name, blob = multipart_file(data, self.headers['Content-Type'], 'stateFile')
            v = STATES[i]; v['data'] = blob; v['updated'] = f"T{len(blob)}-{hashlib.md5(blob).hexdigest()[:6]}"
            LOG.write(f"  STATE-UPLOAD {v['name']} emu={v['emulator']} (update via PUT) {blob!r}\n")
            return self.send(200, dict(id=i, rom_id=v['rom_id'], file_name=v['name'], file_size_bytes=len(blob), emulator=v['emulator'], updated_at=v['updated']))
        self.send(404, {"detail": "nope"})
    def do_POST(self):
        u = urlparse(self.path); p = u.path; q = {k: v[0] for k, v in parse_qs(u.query).items()}
        data = self.body()
        LOG.write(f"POST {self.path}\n"); LOG.flush()
        if p == '/api/client-tokens/exchange':
            code = json.loads(data)['code']
            if code != '12345678': return self.send(404, {"detail": "Invalid or expired code"})
            return self.send(200, {"id": 1, "raw_token": "rmm_smoketoken", "scopes": SCOPES, "device_id": None})
        if not self.authed(): return
        if p == '/api/devices':
            reg = json.loads(data); LOG.write(f"  DEVICE {reg}\n")
            return self.send(201, {"device_id": "dev-" + hashlib.md5(reg['name'].encode()).hexdigest()[:6], "name": reg['name']})
        if p == '/api/sync/negotiate':
            req = json.loads(data); ops = []; dev = req['device_id']
            mine = {(x['rom_id'], x['emulator'], x['slot']): x for x in req['saves']}
            for rid in req['rom_ids']:
                latest = {}
                for i, sv in sorted(SAVES.items()):
                    if sv['rom_id'] == rid and sv['slot'] == 'autosave' and sv['emulator'] in (req.get('emulators') or [sv['emulator']]):
                        latest[sv['emulator']] = (i, sv)
                for emu, (i, sv) in latest.items():
                    loc = mine.pop((rid, emu, 'autosave'), None)
                    base = dict(rom_id=rid, save_id=i, file_name=sv['name'], slot='autosave', emulator=emu)
                    b = BASE.get((dev, rid, emu))
                    if loc is None: ops.append({**base, "action": "download"})
                    elif loc['content_hash'] == sv['hash']: ops.append({**base, "action": "no_op"})
                    elif b == sv['hash']: ops.append({**base, "action": "upload", "save_id": None})   # only we changed
                    elif loc['content_hash'] == b: ops.append({**base, "action": "download"})        # only server changed
                    else: ops.append({**base, "action": "conflict"})                                   # both changed
            for (rid, emu, slot), x in mine.items():
                ops.append({"action": "upload", "rom_id": rid, "save_id": None, "file_name": x['file_name'], "slot": slot, "emulator": emu})
            LOG.write(f"  NEGOTIATE -> {[o['action'] for o in ops]}\n")
            return self.send(200, {"session_id": 5, "operations": ops})
        if p == '/api/states':
            name, blob = multipart_file(data, self.headers['Content-Type'], 'stateFile')
            ex = [i for i, v in STATES.items() if v['name'] == name and v['rom_id'] == int(q['rom_id'])]
            i = ex[0] if ex else NEXT[0] + 1
            if not ex: NEXT[0] += 1
            STATES[i] = dict(rom_id=int(q['rom_id']), emulator=q.get('emulator'), name=name, data=blob, updated=f"T{len(blob)}-{hashlib.md5(blob).hexdigest()[:6]}")
            LOG.write(f"  STATE-UPLOAD {name} emu={q.get('emulator')} {'(update)' if ex else '(new)'} {blob!r}\n")
            return self.send(201, dict(id=i, rom_id=int(q['rom_id']), file_name=name, file_size_bytes=len(blob), emulator=q.get('emulator'), updated_at=STATES[i]['updated']))
        if p == '/api/saves':
            name, blob = multipart_file(data, self.headers['Content-Type'])
            NEXT[0] += 1
            SAVES[NEXT[0]] = dict(rom_id=int(q['rom_id']), emulator=q.get('emulator'), slot=q.get('slot'), name=name, data=blob, hash=romm_hash(blob))
            assert q.get('content_hash') in (None, SAVES[NEXT[0]]['hash']), "content_hash mismatch"
            if q.get('device_id') and q.get('slot'): BASE[(q['device_id'], int(q['rom_id']), q.get('emulator'))] = SAVES[NEXT[0]]['hash']
            import io, zipfile
            try: ents = sorted(n for n in zipfile.ZipFile(io.BytesIO(blob)).namelist() if not n.endswith('/'))
            except zipfile.BadZipFile: ents = None
            LOG.write(f"  UPLOAD slot={q.get('slot')} emu={q.get('emulator')} name={name} bytes={len(blob)} hash={SAVES[NEXT[0]]['hash']} entries={ents}\n")
            return self.send(201, {"id": NEXT[0], "rom_id": int(q['rom_id']), "file_name": name})
        m = re.match(r'/api/sync/sessions/(\d+)/complete', p)
        if m:
            LOG.write(f"  COMPLETE {data.decode()}\n"); return self.send(200, {})
        self.send(404, {"detail": "nope"})

ThreadingHTTPServer(('127.0.0.1', 18765), H).serve_forever()
