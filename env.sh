# Rust 工具链在本项目旁边（非系统安装），source 一次即可用 cargo
# 用法： source /vol1/@appshare/dsh/data/music-tag/env.sh
export RUSTUP_HOME=/vol1/@appshare/dsh/rust-test/rustup
export CARGO_HOME=/vol1/@appshare/dsh/rust-test/cargo
export PATH="/vol1/@appshare/dsh/rust-test/cargo/bin:$PATH"
# CLI 快捷别名（可选，不想用就删掉这行）
alias mt=/vol1/@appshare/dsh/data/music-tag/target/release/music-tag
