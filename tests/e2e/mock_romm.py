"""A small stateful fake of the RomM API: library, covers, downloads, firmware, saves (device sync) and states."""
import hashlib, io, json, re, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs, unquote

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 18766
LOG = open(sys.argv[2] if len(sys.argv) > 2 else '/tmp/e2e/server.log', 'w')
TOKEN = 'rmm_e2etoken'
SCOPES = ["me.read","platforms.read","roms.read","firmware.read","assets.read","assets.write","devices.read","devices.write"]
BIOS = b"FAKE-PS1-BIOS-IMAGE"

def mkrom(i, name, plat, slug, fs_stem=None):
    fs = (fs_stem or name.replace(':', '')) + {'ngc': '.iso', 'psx': '.cue', 'psp': '.iso'}[slug]
    return dict(id=i, name=name, fs_name=fs, fs_name_no_ext=fs.rsplit('.', 1)[0], fs_size_bytes=2_000_000, platform_id=plat,
                platform_slug=slug, platform_fs_slug=slug, has_multiple_files=False, files=[],
                path_cover_small=f"/assets/romm/resources/roms/{plat}/{i}/cover/small.png?ts=1")
ROMS = [mkrom(7, "Super Smash Bros. Melee", 1, 'ngc')]
for n in range(1, 62):
    ROMS.append(mkrom(1000 + n, f"GameCube Title {n:02d}", 1, 'ngc'))
# Spyro exists as three files: USA (103), Europe (113) and Japan (123), one game. As in RomM, each is its own ROM,
# they list each other in sibling_roms (id, name, fs_name_no_tags, fs_name_no_ext, is_main_sibling only), and a
# request with group_by_meta_id=true returns only the one RomM picked to represent them: here 103, the USA copy.
SPYRO = {103: "Spyro the Dragon (USA)", 113: "Spyro the Dragon (Europe) (En,Fr,De)", 123: "Spyro the Dragon (Japan)"}
SPYRO_PRIMARY = 103
ROMS += [mkrom(101, "Final Fantasy VII", 2, 'psx'), mkrom(102, "Crash Bandicoot", 2, 'psx'),
         *[mkrom(i, "Spyro the Dragon", 2, 'psx', fs_stem=stem) for i, stem in SPYRO.items()],
         mkrom(201, "Daxter", 3, 'psp'), mkrom(202, "Lumines", 3, 'psp')]
ROMS.sort(key=lambda r: (r['name'].lower(), r['id']))
for r in ROMS:
    r['sibling_roms'] = [dict(id=i, name=r['name'], fs_name_no_tags="Spyro the Dragon", fs_name_no_ext=stem, is_main_sibling=(i == SPYRO_PRIMARY))
                         for i, stem in SPYRO.items() if r['id'] in SPYRO and i != r['id']]

# RetroAchievements, shaped like RomM's responses. Achievement ids (9001...) deliberately differ from
# badge names (10001...): RomM reports what a user has earned by *badge name*, so a client that matched
# on the achievement id would show nothing earned here.
def _badge(n, lock=False): return f"/assets/romm/resources/roms/1/7/retroachievements/badges/{n}{'_lock' if lock else ''}.png"
RA_ACHIEVEMENTS = [
    dict(ra_id=9000 + i, title=t, description=d, points=pts, badge_id=str(10000 + i), badge_path=_badge(10000 + i), badge_path_lock=_badge(10000 + i, True),
         display_order=i, type=k, num_awarded=100, num_awarded_hardcore=50)
    for i, (t, d, pts, k) in enumerate([("First Blood", "Win your first match", 5, "progression"),
                                        ("Green Hill Clear", "Finish Adventure Mode", 10, "win_condition"),
                                        ("Secret Stage", "Find the hidden stage", 25, "missable")], start=1)]
for r in ROMS:
    if r['id'] == 7:
        r['ra_id'] = 1
        r['merged_ra_metadata'] = dict(first_release_date=1, genres=[], companies=[], hash_match=True, achievements=RA_ACHIEVEMENTS)
ME = dict(id=1, username='e2e', email=None, enabled=True, role='admin', oauth_scopes=SCOPES, avatar_path='', ra_username='e2e_ra',
          ra_progression=dict(total=1, results=[dict(rom_ra_id=1, max_possible=3, num_awarded=1, num_awarded_hardcore=1,
                                                      most_recent_awarded_date='2026-03-04 05:06:07', highest_award_kind=None,
                                                      earned_achievements=[dict(id='10002', date='2026-03-04 05:06:07', date_hardcore='2026-03-04 05:06:07')])]))
PLATS = [
  dict(id=1, slug='ngc', fs_slug='ngc', name='GameCube', rom_count=sum(r['platform_id'] == 1 for r in ROMS), firmware=[]),
  dict(id=2, slug='psx', fs_slug='psx', name='PlayStation', rom_count=5,
       firmware=[dict(id=9, platform_id=2, file_name='scph5501.bin', file_size_bytes=len(BIOS), md5_hash=hashlib.md5(BIOS).hexdigest(), is_verified=True, missing_from_fs=False)]),
  dict(id=3, slug='psp', fs_slug='psp', name='PlayStation Portable', rom_count=2, firmware=[]),
]
BASE, SAVES, STATES, NEXT = {}, {}, {}, [100]

def cover_png(rid):
    from PIL import Image, ImageDraw
    h = (rid * 47) % 360
    img = Image.new('RGB', (300, 400))
    px = img.load()
    for y in range(400):
        for x in range(300):
            px[x, y] = (int(40 + 60 * y / 400), int(30 + (h % 90)), int(90 + 100 * (x / 300)))
    d = ImageDraw.Draw(img)
    d.rectangle((20, 20, 280, 380), outline=(255, 255, 255), width=3)
    d.text((40, 180), f"#{rid}", fill=(255, 255, 255))
    buf = io.BytesIO(); img.save(buf, 'PNG'); return buf.getvalue()

def multipart_file(body, ctype, field):
    boundary = re.search(r'boundary=(.+)', ctype).group(1).encode()
    for part in body.split(b'--' + boundary):
        if (b'name="' + field.encode() + b'"') in part:
            head, _, data = part.partition(b'\r\n\r\n')
            return re.search(rb'filename="([^"]*)"', head).group(1).decode(), data.rsplit(b'\r\n', 1)[0]
    return None, None

def stamp(blob): return f"T{len(blob)}-{hashlib.md5(blob).hexdigest()[:6]}"

class H(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def send(self, code, obj=None, raw=None, ctype=None):
        body = raw if raw is not None else json.dumps(obj if obj is not None else {}).encode()
        self.send_response(code); self.send_header('Content-Length', str(len(body)))
        self.send_header('Content-Type', ctype or ('application/octet-stream' if raw is not None else 'application/json'))
        self.end_headers(); self.wfile.write(body)
    def authed(self):
        if self.headers.get('Authorization') != f'Bearer {TOKEN}':
            self.send(401, {"detail": "Not authenticated"}); return False
        return True
    def body(self): return self.rfile.read(int(self.headers.get('Content-Length') or 0))

    def do_GET(self):
        u = urlparse(self.path); p = unquote(u.path); q = {k: v[0] for k, v in parse_qs(u.query).items()}
        LOG.write(f"GET {self.path}\n"); LOG.flush()
        if not self.authed(): return
        if p == '/api/platforms': return self.send(200, PLATS)
        if p == '/api/users/me': return self.send(200, ME)
        if p == '/api/roms':
            grouped = q.get('group_by_meta_id') == 'true'
            items = [r for r in ROMS if ('platform_ids' not in q or str(r['platform_id']) == q['platform_ids'])
                     and (not q.get('search_term') or q['search_term'].lower() in r['name'].lower())
                     and not (grouped and r['id'] in SPYRO and r['id'] != SPYRO_PRIMARY)]
            off, lim = int(q.get('offset', 0)), int(q.get('limit', 50))
            return self.send(200, dict(items=items[off:off + lim], total=len(items), limit=lim, offset=off))
        m = re.match(r'/api/roms/(\d+)$', p)
        if m: return self.send(200, next(r for r in ROMS if r['id'] == int(m.group(1))))
        m = re.match(r'/api/roms/(\d+)/content/', p)
        if m:   # paced so the progress bar is visible
            total = 2_000_000; self.send_response(200); self.send_header('Content-Length', str(total)); self.end_headers()
            for _ in range(20): self.wfile.write(b'R' * (total // 20)); self.wfile.flush(); time.sleep(0.05)
            return
        if p.startswith('/assets/') and '/retroachievements/badges/' in p: return self.send(200, raw=cover_png(7), ctype='image/png')
        if p.startswith('/assets/'):
            rid = int(re.search(r'/(\d+)/cover/', p).group(1)); return self.send(200, raw=cover_png(rid), ctype='image/png')
        if p == '/api/firmware/9/content/scph5501.bin': return self.send(200, raw=BIOS)
        if p == '/api/states':
            return self.send(200, [dict(id=i, rom_id=v['rom_id'], file_name=v['name'], file_size_bytes=len(v['data']), emulator=v['emulator'], updated_at=v['updated'], missing_from_fs=False)
                                   for i, v in STATES.items() if v['rom_id'] == int(q.get('rom_id', v['rom_id']))])
        m = re.match(r'/api/states/(\d+)/content', p)
        if m: return self.send(200, raw=STATES[int(m.group(1))]['data'])
        m = re.match(r'/api/saves/(\d+)/content', p)
        if m:
            sv = SAVES[int(m.group(1))]
            if q.get('device_id'): BASE[(q['device_id'], sv['rom_id'], sv['emulator'])] = sv['hash']
            return self.send(200, raw=sv['data'])
        self.send(404, {"detail": "nope"})

    def do_PUT(self):
        u = urlparse(self.path); p = u.path; data = self.body()
        if not self.authed(): return
        m = re.match(r'/api/states/(\d+)$', p)
        if m:
            i = int(m.group(1)); _, blob = multipart_file(data, self.headers['Content-Type'], 'stateFile')
            v = STATES[i]; v['data'] = blob; v['updated'] = stamp(blob)
            LOG.write(f"  STATE-UPLOAD {v['name']} (PUT) {blob!r}\n"); LOG.flush()
            return self.send(200, dict(id=i, rom_id=v['rom_id'], file_name=v['name'], file_size_bytes=len(blob), emulator=v['emulator'], updated_at=v['updated']))
        self.send(404, {"detail": "nope"})

    def do_POST(self):
        u = urlparse(self.path); p = u.path; q = {k: v[0] for k, v in parse_qs(u.query).items()}; data = self.body()
        LOG.write(f"POST {self.path}\n"); LOG.flush()
        if p == '/api/client-tokens/exchange':
            if json.loads(data)['code'] != '12345678': return self.send(404, {"detail": "Invalid or expired code"})
            return self.send(200, dict(id=1, raw_token=TOKEN, scopes=SCOPES, device_id=None))
        if not self.authed(): return
        if re.match(r'/api/users/\d+/ra/refresh$', p): return self.send(403, {"detail": "Forbidden"})
        if p == '/api/devices':
            reg = json.loads(data); LOG.write(f"  DEVICE {reg}\n"); LOG.flush()
            return self.send(201, dict(device_id='dev-' + hashlib.md5(reg['name'].encode()).hexdigest()[:6], name=reg['name']))
        if p == '/api/states':
            name, blob = multipart_file(data, self.headers['Content-Type'], 'stateFile')
            ex = [i for i, v in STATES.items() if v['name'] == name and v['rom_id'] == int(q['rom_id'])]
            i = ex[0] if ex else NEXT[0] + 1
            if not ex: NEXT[0] += 1
            STATES[i] = dict(rom_id=int(q['rom_id']), emulator=q.get('emulator'), name=name, data=blob, updated=stamp(blob))
            LOG.write(f"  STATE-UPLOAD {name} emu={q.get('emulator')} {blob!r}\n"); LOG.flush()
            return self.send(201, dict(id=i, rom_id=int(q['rom_id']), file_name=name, file_size_bytes=len(blob), emulator=q.get('emulator'), updated_at=STATES[i]['updated']))
        if p == '/api/sync/negotiate':
            req = json.loads(data); ops = []; dev = req['device_id']
            mine = {(x['rom_id'], x['emulator'], x['slot']): x for x in req['saves']}
            for rid in req['rom_ids']:
                latest = {}
                for i, sv in sorted(SAVES.items()):
                    if sv['rom_id'] == rid and sv['slot'] == 'autosave' and sv['emulator'] in (req['emulators'] or [sv['emulator']]): latest[sv['emulator']] = (i, sv)
                for emu, (i, sv) in latest.items():
                    loc = mine.pop((rid, emu, 'autosave'), None)
                    base = dict(rom_id=rid, save_id=i, file_name=sv['name'], slot='autosave', emulator=emu); b = BASE.get((dev, rid, emu))
                    if loc is None: ops.append({**base, "action": "download"})
                    elif loc['content_hash'] == sv['hash']: ops.append({**base, "action": "no_op"})
                    elif b == sv['hash']: ops.append({**base, "action": "upload", "save_id": None})
                    elif loc['content_hash'] == b: ops.append({**base, "action": "download"})
                    else: ops.append({**base, "action": "conflict"})
            for (rid, emu, slot), x in mine.items():
                ops.append(dict(action="upload", rom_id=rid, save_id=None, file_name=x['file_name'], slot=slot, emulator=emu))
            LOG.write(f"  NEGOTIATE -> {[o['action'] for o in ops]}\n"); LOG.flush()
            return self.send(200, dict(session_id=5, operations=ops))
        if p == '/api/saves':
            name, blob = multipart_file(data, self.headers['Content-Type'], 'saveFile'); NEXT[0] += 1
            SAVES[NEXT[0]] = dict(rom_id=int(q['rom_id']), emulator=q.get('emulator'), slot=q.get('slot'), name=name, data=blob, hash=hashlib.md5(blob).hexdigest())
            if q.get('device_id') and q.get('slot'): BASE[(q['device_id'], int(q['rom_id']), q.get('emulator'))] = SAVES[NEXT[0]]['hash']
            LOG.write(f"  SAVE-UPLOAD slot={q.get('slot')} {name} {len(blob)}B\n"); LOG.flush()
            return self.send(201, dict(id=NEXT[0], rom_id=int(q['rom_id']), file_name=name))
        if re.match(r'/api/sync/sessions/\d+/complete', p):
            LOG.write(f"  COMPLETE {data.decode()[:200]}\n"); LOG.flush(); return self.send(200, {})
        self.send(404, {"detail": "nope"})

ThreadingHTTPServer(('127.0.0.1', PORT), H).serve_forever()
