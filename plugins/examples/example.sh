#!/bin/sh
# @music-robot
# {
#   "name": "example-sh",
#   "kind": "scraper",
#   "protocol": 1,
#   "capabilities": ["metadata"],
#   "timeout_ms": 10000,
#   "max_concurrency": 1
# }
# @end
#
# 示例插件（POSIX sh）—— 单文件、零外部依赖、daemon 模式。
# 协议约定见同目录的 example.js（响应必须回显 id 与 action、stdout 只放协议 JSON）。
#
# sh 里没有 JSON 库，这里用 sed 抠字段；逻辑一复杂就该改用 .js / .py。

while IFS= read -r line; do
  # 空行忽略
  if [ -z "$line" ]; then
    continue
  fi

  # 抠出 id 与 action（演示用；真实插件请用正经 JSON 解析）
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  action=$(printf '%s' "$line" | sed -n 's/.*"action":"\([^"]*\)".*/\1/p')

  if [ "$action" = "scrape" ]; then
    printf '{"id":"%s","protocol":1,"action":"scrape","ok":true,"candidates":[{"confidence":0.75,"source":"example-sh"}]}\n' "$id"
  else
    printf '{"id":"%s","protocol":1,"action":"%s","ok":false,"error":{"code":"NOT_FOUND","message":"example-sh 不支持该 action"}}\n' "$id" "$action"
  fi
done
