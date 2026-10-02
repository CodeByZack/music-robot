#!/usr/bin/env python3
# @music-robot
# {
#   "name": "example-py",
#   "kind": "scraper",
#   "protocol": 1,
#   "capabilities": ["metadata"],
#   "timeout_ms": 10000,
#   "max_concurrency": 1
# }
# @end
#
# 示例插件（Python）—— 单文件、零外部依赖、daemon 模式。
# 协议约定见同目录的 example.js（响应必须回显 id 与 action、stdout 只放协议 JSON）。

import json
import sys


def respond(obj):
    sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def handle(req):
    base = {
        "id": req.get("id", ""),
        "protocol": 1,
        "action": req.get("action", ""),
    }

    if req.get("action") == "scrape":
        song = req.get("song") or {}
        title = song.get("title") or "未知标题"
        base.update({
            "ok": True,
            "candidates": [
                {
                    "confidence": 0.85,
                    "source": "example-py",
                    "matched": {"id": "ex-py-1", "title": title},
                    "tags": {"title": title, "artist": "示例歌手"},
                }
            ],
        })
        respond(base)
        return

    base.update({
        "ok": False,
        "error": {
            "code": "NOT_FOUND",
            "message": "example-py 不支持 action: " + str(req.get("action")),
        },
    })
    respond(base)


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except ValueError as exc:
            respond({
                "id": "", "protocol": 1, "action": "scrape", "ok": False,
                "error": {"code": "BAD_REQUEST", "message": "请求不是合法 JSON: %s" % exc},
            })
            continue
        handle(req)


if __name__ == "__main__":
    main()
