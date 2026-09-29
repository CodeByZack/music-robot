#!/usr/bin/env python3
"""music-robot HTTP API 端到端验证。

起一个**临时服务**（独立临时数据库 + 临时曲库目录），把每个接口打一遍并逐条断言，
最后打印通过 / 失败统计。它既是「接口确实能用」的证明，也是可重复运行的回归测试。

用法:
    python3 scripts/api_test.py                # 用 target/debug/music-robot
    python3 scripts/api_test.py --bin PATH     # 指定二进制
    python3 scripts/api_test.py --keep         # 结束后保留临时目录（排查用）

约定:
    · 只用标准库（urllib），不装任何依赖；
    · 所有断言都打到真实 HTTP 响应上，不做「只要不报错就算过」的弱断言；
    · 失败不会中断全局，而是累计到最后的统计里。
"""

import argparse
import json
import os
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIXTURES = os.path.join(REPO, "fixtures")

# 挑样本：一个带封面（用于封面接口）、一个不带封面、一个 flac
SAMPLE_FILES = [
    "最美情侣-白小白.mp3",   # has_cover: true
    "华夏传说 - 凤凰传奇.mp3",  # has_cover: false
    "牵丝戏 - 白兀.flac",
]

PASS = 0
FAIL = 0
FAILED_NAMES = []


def ok(name, extra=""):
    global PASS
    PASS += 1
    print("  \033[32m✓\033[0m %s%s" % (name, ("  [%s]" % extra) if extra else ""))


def bad(name, detail):
    global FAIL
    FAIL += 1
    FAILED_NAMES.append(name)
    print("  \033[31m✗\033[0m %s\n      %s" % (name, detail))


def brief(v, limit=120):
    # 把要打印的值压短 —— 音频 / 封面这类二进制响应体绝不能原样刷屏。
    r = repr(v)
    if len(r) > limit:
        return r[:limit] + "...(共 %d 字符)" % len(r)
    return r


def check(name, actual, expected):
    if actual == expected:
        ok(name, "= %s" % brief(expected))
    else:
        bad(name, "期望 %s，实际 %s" % (brief(expected), brief(actual)))


def check_true(name, cond, detail=""):
    if cond:
        ok(name)
    else:
        bad(name, detail or "条件不成立")


def lower_headers(msg):
    """把响应头名统一转小写。

    ⚠️ axum / hyper 发出的头名是**小写**的（content-type、accept-ranges…），
    而 Python 的 http.client 保留原样。不统一大小写的话 hd.get("content-type")
    永远取不到值 —— 响应头断言会集体假失败（本脚本第一版就栽在这）。
    """
    return {k.lower(): v for k, v in msg.items()}


class Client:
    def __init__(self, base):
        self.base = base
        self.token = None

    def req(self, method, path, body=None, headers=None, raw=False):
        url = self.base + path
        data = None
        h = dict(headers or {})
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            h["Content-Type"] = "application/json"
        if self.token:
            h["Authorization"] = "Bearer " + self.token
        r = urllib.request.Request(url, data=data, headers=h, method=method)
        try:
            with urllib.request.urlopen(r, timeout=30) as resp:
                payload = resp.read()
                return resp.status, lower_headers(resp.headers), payload
        except urllib.error.HTTPError as e:
            return e.code, lower_headers(e.headers), e.read()

    def json(self, method, path, body=None, headers=None):
        st, hd, payload = self.req(method, path, body, headers)
        try:
            return st, hd, json.loads(payload.decode("utf-8"))
        except Exception:
            return st, hd, None


def newest_source_mtime():
    # src/ 下所有 .rs 里最新的 mtime。
    newest = 0.0
    for root, _dirs, files in os.walk(os.path.join(REPO, "src")):
        for fn in files:
            if fn.endswith(".rs"):
                newest = max(newest, os.path.getmtime(os.path.join(root, fn)))
    return newest


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def wait_healthz(base, timeout=20.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(base + "/healthz", timeout=2) as r:
                if r.status == 200:
                    return True
        except Exception:
            time.sleep(0.15)
    return False


def section(title):
    print("\n\033[1m%s\033[0m" % title)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=os.path.join(REPO, "target", "debug", "music-robot"))
    ap.add_argument("--keep", action="store_true")
    args = ap.parse_args()

    if not os.path.isfile(args.bin):
        print("找不到二进制：%s\n先跑 cargo build --bin music-robot" % args.bin, file=sys.stderr)
        return 2

    # ⚠️ 防呆：二进制必须比 src/ 下任何源码都新。
    # 吃过一次亏 —— 代码改完忘了重新构建，脚本对着旧二进制跑，
    # 新接口全部返回旧行为，一度被误判成实现有 bug。
    bin_mtime = os.path.getmtime(args.bin)
    src_mtime = newest_source_mtime()
    if bin_mtime < src_mtime - 1.0:
        print("二进制比源码旧，拒绝运行（否则测的是旧版本，结论无意义）：", file=sys.stderr)
        print("  二进制  : %s" % time.strftime("%H:%M:%S", time.localtime(bin_mtime)), file=sys.stderr)
        print("  最新源码: %s" % time.strftime("%H:%M:%S", time.localtime(src_mtime)), file=sys.stderr)
        print("  请先跑: cargo build --bin music-robot", file=sys.stderr)
        return 2

    work = tempfile.mkdtemp(prefix="music-robot-api-test-")
    music = os.path.join(work, "music")
    os.makedirs(music)
    db_path = os.path.join(work, "music.db")
    cache_dir = os.path.join(work, "transcode-cache")
    # ⚠️ 必须隔离转码缓存目录！默认是 ~/.cache/music-robot/transcode，
    # 而本脚本用的是**假 ffmpeg**（产物是垃圾字节）。不隔离就会把用户真实的
    # 缓存目录写脏 —— 而且因为缓存键只有 song_id+audio_hash，
    # 之后用真 ffmpeg 也只会命中那份脏缓存。这条踩过一次，务必保留。
    # config.rs 目前没有 MR_CACHE_DIR 环境变量，只能用配置文件覆盖。
    cfg_file = os.path.join(work, "config.json")
    with open(cfg_file, "w", encoding="utf-8") as fh:
        json.dump({"audio": {"cache_dir": cache_dir}}, fh)

    # 拷真实样本进临时曲库（扫描出真实标签）
    copied = []
    for name in SAMPLE_FILES:
        src = os.path.join(FIXTURES, name)
        if os.path.isfile(src):
            shutil.copy2(src, os.path.join(music, name))
            copied.append(name)
    if not copied:
        print("fixtures/ 里没有可用样本，无法验证", file=sys.stderr)
        return 2
    print("临时目录: %s" % work)
    print("曲库样本: %s" % ", ".join(copied))

    # 造一个假的 ffmpeg：每次被调用就往计数文件追加一行，并产出一个（假的）mp3。
    # 转码接口的「二次命中不调 ffmpeg」就靠它提供硬证据。
    fake_ffmpeg = os.path.join(work, "fake-ffmpeg.sh")
    ffmpeg_calls = os.path.join(work, "ffmpeg-calls.txt")
    with open(fake_ffmpeg, "w", encoding="utf-8") as f:
        f.write("#!/bin/sh\n"
                "echo call >> %s\n"
                "# 最后一个参数是输出文件；直接写点字节冒充转码结果\n"
                "for last; do :; done\n"
                "printf 'ID3fake-mp3-bytes' > \"$last\"\n" % ffmpeg_calls)
    os.chmod(fake_ffmpeg, 0o755)

    port = free_port()
    base = "http://127.0.0.1:%d" % port
    env = dict(os.environ)
    env.update({
        "MR_DATABASE_PATH": db_path,
        "MR_LIBRARY_ROOTS": music,
        "MR_JWT_SECRET": "api-test-secret",
        "MR_FFMPEG_PATH": fake_ffmpeg,
    })
    env.setdefault("MR_LOG_LEVEL", "warn")

    log = open(os.path.join(work, "server.log"), "w")
    proc = subprocess.Popen([args.bin, "serve", "--host", "127.0.0.1", "--port", str(port), "--config", cfg_file],
                            env=env, stdout=log, stderr=subprocess.STDOUT)

    rc = 1
    try:
        if not wait_healthz(base):
            print("\n服务未在 20 秒内就绪，日志：", file=sys.stderr)
            log.flush()
            print(open(os.path.join(work, "server.log"), encoding="utf-8", errors="replace").read(), file=sys.stderr)
            return 1

        c = Client(base)
        c2 = Client(base)   # 第二个用户

        # ───────────────────────── 健康检查 / 鉴权 ─────────────────────────
        section("1. 健康检查与鉴权")
        st, _, body = c.json("GET", "/healthz")
        check("GET /healthz → 200", st, 200)
        check_true("健康检查返回 status=ok", (body or {}).get("status") == "ok", repr(body))

        st, _, body = c.json("GET", "/api/library")
        check("未登录访问曲库 → 401", st, 401)

        st, _, body = c.json("POST", "/api/auth/register",
                             {"username": "alice", "password": "password123"})
        check("注册首个用户 → 201", st, 201)
        check("首个用户是 admin", ((body or {}).get("user") or {}).get("role"), "admin")

        st, _, body = c.json("POST", "/api/auth/register",
                             {"username": "bob", "password": "password123"})
        check("注册第二个用户 → 201", st, 201)
        check("第二个用户是 user", ((body or {}).get("user") or {}).get("role"), "user")

        st, _, body = c.json("POST", "/api/auth/login",
                             {"username": "alice", "password": "password123"})
        check("登录 → 200", st, 200)
        c.token = (body or {}).get("token") or ""
        check_true("登录返回非空 token", len(c.token) > 20, "token 太短: %r" % c.token)

        st, _, body = c2.json("POST", "/api/auth/login",
                              {"username": "bob", "password": "password123"})
        c2.token = (body or {}).get("token") or ""

        st, _, body = c.json("GET", "/api/auth/me")
        check("GET /api/auth/me → 200", st, 200)
        check("me 返回的是 alice", ((body or {}).get("user") or {}).get("username"), "alice")

        st, _, _ = c2.json("GET", "/api/admin/ping")
        check("普通用户访问 admin 路由 → 403", st, 403)
        st, _, _ = c.json("GET", "/api/admin/ping")
        check("管理员访问 admin 路由 → 200", st, 200)

        # ───────────────────────── 扫描入库 ─────────────────────────
        section("2. 扫描入库（S5/S17）")
        st, _, body = c.json("POST", "/api/scan")
        check("触发扫描 → 202", st, 202)
        batch = (body or {}).get("batch_id")
        check_true("返回非空 batch_id", bool(batch), repr(body))

        # 任务还在跑时重复触发应当被单例锁挡住（若已跑完则跳过，避免偶发）
        st2, _, _ = c.json("POST", "/api/scan")
        if st2 == 409:
            ok("扫描进行中重复触发 → 409（单例锁）")
        else:
            ok("扫描已完成，重复触发未被挡（跳过 409 断言，st=%s）" % st2)

        status = None
        for _ in range(100):
            st, _, body = c.json("GET", "/api/scan/%s" % batch)
            status = (body or {}).get("status")
            if status in ("done", "failed"):
                break
            time.sleep(0.2)
        check("扫描任务最终状态 = done", status, "done")
        check_true("扫描报告 total > 0", ((body or {}).get("total") or 0) > 0, repr(body))

        # ───────────────────────── 曲库接口 ─────────────────────────
        section("3. 曲库接口（S16）")
        st, _, body = c.json("GET", "/api/library")
        check("GET /api/library → 200", st, 200)
        total = (body or {}).get("total") or 0
        check_true("库里有歌（total >= 3）", total >= 3, "total=%r" % total)
        first_id = ((body or {}).get("items") or [{}])[0].get("id")
        check_true("列表项不含 file_path（不泄漏服务器路径）",
                   "file_path" not in json.dumps(body), "响应里出现了 file_path")

        st, _, _ = c.json("GET", "/api/library?page=0")
        check("分页 page=0 → 400", st, 400)
        st, _, _ = c.json("GET", "/api/library?page_size=999999")
        check("分页 page_size 超上限 → 400", st, 400)
        st, _, body = c.json("GET", "/api/library?page=99")
        check("越界页 → 200", st, 200)
        check("越界页返回空数组", (body or {}).get("items"), [])

        st, _, _ = c.json("GET", "/api/library?sort=id;DROP%20TABLE%20songs--")
        check("排序注入 → 400", st, 400)
        # 表还在吗 —— 直接查库确认
        con = sqlite3.connect(db_path)
        n = con.execute("select count(*) from songs").fetchone()[0]
        con.close()
        check_true("注入后 songs 表仍在且有数据", n >= 3, "songs 行数=%r" % n)

        st, _, body = c.json("GET", "/api/songs/%s" % first_id)
        check("GET /api/songs/{id} → 200", st, 200)
        st, _, _ = c.json("GET", "/api/songs/999999")
        check("不存在的歌 → 404", st, 404)

        st, _, body = c.json("GET", "/api/search?q=%E7%99%BD")
        check("搜索 → 200", st, 200)
        st, _, body = c.json("GET", "/api/search?q=zzzz-nope-nothing")
        check("搜不到 → 200", st, 200)
        check("搜不到返回空数组", (body or {}).get("items"), [])

        # ───────────────────────── 音频流（Range） ─────────────────────────
        section("4. 音频流 Range（S18）")
        st, hd, payload = c.req("GET", "/api/stream/%s" % first_id)
        check("无 Range → 200", st, 200)
        check_true("带 Accept-Ranges: bytes", hd.get("accept-ranges") == "bytes", repr(hd.get("accept-ranges")))
        check_true("Content-Length 大于 0", int(hd.get("content-length", "0")) > 0, repr(hd.get("Content-Length")))
        full_len = len(payload)

        st, hd, payload = c.req("GET", "/api/stream/%s" % first_id, headers={"Range": "bytes=0-99"})
        check("Range bytes=0-99 → 206", st, 206)
        check("Range 返回正好 100 字节", len(payload), 100)
        check("Content-Range 正确", hd.get("content-range"), "bytes 0-99/%d" % full_len)

        st, hd, _ = c.req("GET", "/api/stream/%s" % first_id, headers={"Range": "bytes=%d-" % full_len})
        check("越界 Range → 416", st, 416)
        check("416 带 Content-Range: bytes */len", hd.get("content-range"), "bytes */%d" % full_len)

        st, _, _ = c.req("GET", "/api/stream/%s" % first_id, headers={"Range": "bytes=0-1,3-4"})
        check("多段 Range → 416", st, 416)

        # ───────────────────────── 封面 ─────────────────────────
        section("5. 封面（S20）")
        # 找那首带封面的
        with_cover = None
        for item in (body or {}).get("items") or []:
            pass
        st, _, lib = c.json("GET", "/api/library?page_size=200")
        for item in (lib or {}).get("items") or []:
            st, hd, payload = c.req("GET", "/api/songs/%s/cover" % item["id"])
            if st == 200:
                with_cover = (item["id"], hd, payload)
                break
        check_true("至少有一首歌能取到封面", with_cover is not None,
                   "所有歌的封面接口都返回了非 200")
        if with_cover:
            _, hd, payload = with_cover
            check_true("封面 Content-Type 是 image/*",
                       (hd.get("content-type") or "").startswith("image/"), repr(hd.get("content-type")))
            check_true("封面字节非空", len(payload) > 1000, "只有 %d 字节" % len(payload))
        st, _, _ = c.req("GET", "/api/songs/999999/cover")
        check("不存在的歌取封面 → 404", st, 404)

        # ───────────────────────── 转码缓存 ─────────────────────────
        section("6. 转码缓存（S19）")

        def count_ffmpeg_calls():
            try:
                with open(ffmpeg_calls, encoding="utf-8") as fh:
                    return len([ln for ln in fh if ln.strip()])
            except FileNotFoundError:
                return 0

        # 挑一首 flac（转码最有意义的场景）；没有就用第一首
        target = None
        for item in (lib or {}).get("items") or []:
            if (item.get("format") or "").lower() == "flac":
                target = item["id"]
                break
        if target is None:
            target = first_id

        before = count_ffmpeg_calls()
        st, hd, payload = c.req("GET", "/api/stream/%s?format=mp3" % target)
        check("?format=mp3 → 200", st, 200)
        check("转码响应 Content-Type = audio/mpeg", hd.get("content-type"), "audio/mpeg")
        check_true("转码响应非空", len(payload) > 0, "0 字节")
        check("ffmpeg 被调用恰好 1 次", count_ffmpeg_calls(), before + 1)

        st, hd, payload2 = c.req("GET", "/api/stream/%s?format=mp3" % target)
        check("二次转码请求 → 200", st, 200)
        check("二次命中缓存：ffmpeg 调用次数不变（硬证据）", count_ffmpeg_calls(), before + 1)
        check("两次响应字节完全一致", payload2, payload)

        st, _, _ = c.req("GET", "/api/stream/%s?format=ogg" % target)
        check("不支持的 format → 400", st, 400)

        anon = Client(base)
        st, _, _ = anon.req("GET", "/api/stream/%s?format=mp3" % target)
        check("转码未登录 → 401", st, 401)
        check("未登录不产生转码调用", count_ffmpeg_calls(), before + 1)

        st, hd, _ = c.req("GET", "/api/stream/%s" % target)
        check("不带 format → 200（直传，S18 行为不变）", st, 200)
        check("直传不触发转码", count_ffmpeg_calls(), before + 1)

        # ───────────────────────── 播放列表 ─────────────────────────
        section("7. 播放列表（S22）")

        st, _, body = c.json("POST", "/api/playlists", {"name": "测试歌单", "is_public": False})
        check("新建歌单 → 201", st, 201)
        pl_id = ((body or {}).get("playlist") or {}).get("id")
        check_true("返回歌单 id", isinstance(pl_id, int), repr(body))

        st, _, body = c.json("GET", "/api/playlists")
        check("歌单列表 → 200", st, 200)
        ids = [x.get("id") for x in (body or {}).get("items") or []]
        check_true("列表含刚建的歌单", pl_id in ids, repr(ids))

        st, _, body = c.json("POST", "/api/playlists/%s/items" % pl_id, {"song_id": first_id})
        check("加歌 → 201", st, 201)
        check_true("加歌 added=true", (body or {}).get("added") is True, repr(body))
        st, _, body = c.json("POST", "/api/playlists/%s/items" % pl_id, {"song_id": first_id})
        check("重复加歌 → 200（幂等）", st, 200)
        check_true("重复加歌 added=false", (body or {}).get("added") is False, repr(body))

        st, _, body = c.json("GET", "/api/playlists/%s" % pl_id)
        check("歌单详情 → 200", st, 200)
        tracks = (body or {}).get("songs") or []
        check("详情里恰好一首（幂等生效）", len(tracks), 1)
        check_true("详情曲目不含内部字段", "file_path" not in json.dumps(body), "泄漏了 file_path")

        st, _, _ = c.json("POST", "/api/playlists/%s/items" % pl_id, {"song_id": 999999})
        check("加不存在的歌 → 404", st, 404)

        # 私有歌单：外人看不见，且与「不存在」不可区分
        st_a, _, body_a = c2.json("GET", "/api/playlists/%s" % pl_id)
        check("外人读私有歌单 → 404", st_a, 404)
        st_b, _, body_b = c2.json("GET", "/api/playlists/999999")
        check_true("私有与不存在不可区分（防存在性泄漏）", body_a == body_b, "两者响应不同")
        st, _, _ = c2.json("DELETE", "/api/playlists/%s" % pl_id)
        check("外人删私有歌单 → 404", st, 404)

        # 改成公开：外人能读，但仍不能改
        st, _, _ = c.json("PUT", "/api/playlists/%s" % pl_id, {"is_public": True})
        check("改为公开 → 200", st, 200)
        st, _, _ = c2.json("GET", "/api/playlists/%s" % pl_id)
        check("外人读公开歌单 → 200", st, 200)
        st, _, _ = c2.json("PUT", "/api/playlists/%s" % pl_id, {"name": "被改名"})
        check("外人改公开歌单 → 403（不是 404）", st, 403)

        anon2 = Client(base)
        st, _, _ = anon2.json("GET", "/api/playlists")
        check("未登录看歌单 → 401", st, 401)

        # 删除级联：直接查库确认曲目没了
        st, _, _ = c.json("DELETE", "/api/playlists/%s" % pl_id)
        check("owner 删歌单 → 200", st, 200)
        con = sqlite3.connect(db_path)
        left = con.execute("select count(*) from playlist_items where playlist_id=?", (pl_id,)).fetchone()[0]
        con.close()
        check("删歌单后曲目被级联清除", left, 0)
        # ───────────────────────── 播放周边 ─────────────────────────
        section("8. 播放周边（S21）")

        # 记录播放
        st, _, _ = c.json("POST", "/api/history", {"song_id": first_id, "duration_listened_ms": 30000})
        check("记录播放 → 201", st, 201)
        st, _, body = c.json("GET", "/api/history?limit=10")
        check("最近播放 → 200", st, 200)
        check_true("历史里含刚记的那条", any(x.get("song_id") == first_id for x in (body or {}).get("items") or []), repr(body)[:200])
        st, _, _ = c.json("POST", "/api/history", {"song_id": 999999})
        check("记录不存在的歌 → 404", st, 404)
        st, _, _ = c.json("GET", "/api/history?limit=0")
        check("历史分页非法参数 → 400", st, 400)

        # 收藏幂等
        st, _, body = c.json("POST", "/api/favorites/%s" % first_id)
        check("收藏 → 201", st, 201)
        st, _, body = c.json("POST", "/api/favorites/%s" % first_id)
        check("重复收藏 → 200（幂等）", st, 200)
        st, _, body = c.json("GET", "/api/favorites")
        check("收藏列表 → 200", st, 200)
        n_fav = len([x for x in (body or {}).get("items") or [] if x.get("id") == first_id])
        check("同一首只出现一次", n_fav, 1)

        # 断点续播：写进 settings 再读回来
        key = "resume:%s" % first_id
        st, _, _ = c.json("PUT", "/api/settings", {key: "42000"})
        check("写设置 → 200", st, 200)
        st, _, body = c.json("GET", "/api/settings")
        check("读设置 → 200", st, 200)
        smap = (body or {}).get("settings") or {}
        check("断点续播位置原样读回", smap.get(key), "42000")
        st, _, _ = c.json("PUT", "/api/settings", {key: None})
        check("null 值删除该键 → 200", st, 200)
        st, _, _ = c.json("PUT", "/api/settings", {"k" * 200: "v"})
        check("超长键 → 400", st, 400)

        # 跨用户隔离
        st, _, body = c2.json("GET", "/api/history?limit=10")
        mine = (body or {}).get("items") or []
        check("B 看不到 A 的播放历史", len(mine), 0)
        st, _, body = c2.json("GET", "/api/favorites")
        check("B 看不到 A 的收藏", len((body or {}).get("items") or []), 0)
        st, _, body = c2.json("GET", "/api/settings")
        items2 = (body or {}).get("settings") or {}
        check("B 看不到 A 的设置", len(items2), 0)

        anon3 = Client(base)
        st, _, _ = anon3.json("GET", "/api/history")
        check("未登录看历史 → 401", st, 401)
        # ───────────────────────── 歌曲请求 ─────────────────────────
        section("9. 歌曲请求（S23）")

        # 归一化：大小写 + 全角 + 空格差异应合并成同一请求
        st, _, b1 = c.json("POST", "/api/requests", {"title": "Numb", "artist": "Linkin Park"})
        check("提交点歌 → 201", st, 201)
        check_true("首次提交 created=true", ((b1 or {}).get("created") is True), repr(b1))
        rid = ((b1 or {}).get("request") or {}).get("id")
        check_true("返回请求 id", isinstance(rid, int), repr(b1))

        # 同一用户重复提交：幂等（不新增行、不重复投票）
        st, _, b2 = c.json("POST", "/api/requests", {"title": "NUMB", "artist": "  linkin park  "})
        check("同人重复提交 → 200", st, 200)
        check_true("created=false（合并到已有）", (b2 or {}).get("created") is False, repr(b2))
        check_true("voted=false（不重复投票）", (b2 or {}).get("voted") is False, repr(b2))
        check("合并到同一个请求 id", ((b2 or {}).get("request") or {}).get("id"), rid)

        # 另一个用户提交全角写法：应合并，且新增一票
        st, _, b3 = c2.json("POST", "/api/requests", {"title": "ＮＵＭＢ", "artist": "Ｌｉｎｋｉｎ　Ｐａｒｋ"})
        check("他人提交全角写法 → 200", st, 200)
        check_true("created=false（归一化后同曲）", (b3 or {}).get("created") is False, repr(b3))
        check_true("voted=true（新增一票）", (b3 or {}).get("voted") is True, repr(b3))
        check("票数 = 2", ((b3 or {}).get("request") or {}).get("vote_count"), 2)

        # 列表按票数降序
        st, _, lst = c.json("GET", "/api/requests")
        check("admin 列点歌 → 200", st, 200)
        items = (lst or {}).get("items") or []
        check_true("列表含刚提交的请求", any(x.get("id") == rid for x in items), repr(lst)[:200])
        check_true("列表项不含 dedup_key（不暴露内部字段）", "dedup_key" not in json.dumps(lst), "泄漏了 dedup_key")

        # 普通用户只看得到自己的
        # song_requests.user_id 是「首个发起人」（画布 tb_req_t），bob 只是投票者，
        # 所以「我提交的」里本来就不该有 alice 那条 —— 让 bob 真正发起一条再验隔离。
        st, _, bb = c2.json("POST", "/api/requests", {"title": "bob-only-song", "artist": "bob"})
        check("bob 自己发起一条 → 201", st, 201)
        bob_rid = ((bb or {}).get("request") or {}).get("id")
        st, _, mine = c2.json("GET", "/api/requests")
        check("普通用户列点歌 → 200", st, 200)
        ids2 = [x.get("id") for x in (mine or {}).get("items") or []]
        check("普通用户只看到自己发起的", ids2, [bob_rid])
        check_true("看不到 alice 发起的", rid not in ids2, "看到了别人的请求")
        st, _, _ = c2.json("GET", "/api/requests?status=pending")
        check("普通用户按状态筛 → 403", st, 403)

        # 状态机与管理员操作
        st, _, _ = c2.json("PATCH", "/api/requests/%s" % rid, {"status": "processing"})
        check("普通用户改状态 → 403", st, 403)
        st, _, _ = c.json("PATCH", "/api/requests/%s" % rid, {"status": "bogus"})
        check("非法状态值 → 400", st, 400)
        st, _, _ = c.json("PATCH", "/api/requests/%s" % rid, {"status": "processing"})
        check("pending→processing → 200", st, 200)
        st, _, _ = c.json("PATCH", "/api/requests/%s" % rid, {"status": "pending"})
        check("processing→pending（回退）→ 400", st, 400)

        st, _, body = c.json("POST", "/api/requests/%s/link" % rid, {"song_id": 999999})
        check("link 到不存在的歌 → 404", st, 404)
        st, _, body = c.json("POST", "/api/requests/%s/link" % rid, {"song_id": first_id})
        check("link 到已有歌 → 200", st, 200)
        check("link 后状态为 done", ((body or {}).get("request") or {}).get("status"), "done")

        # fetch 要的是 **provider（下载）** 插件，该 kind 尚未实现，所以必须诚实报
        # 503，不许假装成功。注意文案里点名的是 provider 而不是「刮削插件」——
        # 刮削（scraper）那条线早已接通，含糊的文案会把排查方向带偏。
        st, _, bf = c.json("POST", "/api/requests/%s/fetch" % rid)
        check("fetch 无 provider 插件 → 503", st, 503)
        check("fetch 错误码 SERVICE_UNAVAILABLE", ((bf or {}).get("error") or {}).get("code"), "SERVICE_UNAVAILABLE")
        check(
            "fetch 文案点名 provider（不是「刮削插件」）",
            "provider" in (((bf or {}).get("error") or {}).get("message") or ""),
            True,
        )

        anon4 = Client(base)
        st, _, _ = anon4.json("GET", "/api/requests")
        check("未登录看请求 → 401", st, 401)
        # ───────────────────────── 汇总 ─────────────────────────
        rc = 0 if FAIL == 0 else 1
    finally:
        try:
            proc.send_signal(signal.SIGTERM)
            proc.wait(timeout=8)
        except Exception:
            try:
                proc.kill()
            except Exception:
                pass
        log.close()
        if rc == 0 and not args.keep:
            shutil.rmtree(work, ignore_errors=True)
        else:
            print("\n临时目录保留在: %s" % work)
            print("服务日志: %s" % os.path.join(work, "server.log"))

    section("结果")
    print("  通过 %d，失败 %d" % (PASS, FAIL))
    if FAILED_NAMES:
        print("  失败项:")
        for n in FAILED_NAMES:
            print("    · %s" % n)
    return rc


if __name__ == "__main__":
    sys.exit(main())
