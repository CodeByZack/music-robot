//! APEv2 读侧测试 —— 用**合成字节**构造（TS 仓库无 APE 样本，差分对拍覆盖不到这条路径）。
//! 手法与 TS roundtrip.test.ts 一致：手工摆规范字节，不使用被测写入器。
use music_robot::tag::read::apev2::{find_ape_footer, parse_ape_tag};

/// 构造一个最小 APEv2 tag（footer-only，无 header），items 为 (key,value) 列表。
fn build_ape(items: &[(&str, &str)], with_id3v1_tail: bool) -> Vec<u8> {
    let mut body: Vec<u8> = Vec::new();
    for (k, v) in items {
        body.extend_from_slice(&(v.len() as u32).to_le_bytes()); // valueSize LE
        body.extend_from_slice(&0u32.to_le_bytes());            // flags=0 → UTF-8 文本
        body.extend_from_slice(k.as_bytes()); body.push(0);      // key + NUL
        body.extend_from_slice(v.as_bytes());
    }
    // ⚠️ size 语义：APEv2 规范要求 "items 区 + footer(32)"，但**参照实现（TS apev2.ts）
    //   按 items-only 定位 tagRegion**（实测：写 items+32 时 TS 返回 null）。
    //   为保持移植保真，fixture 跟随参照实现。差异已记入 HANDOFF.md §6.4 第 2 条。
    let footer_size = body.len();
    let mut f: Vec<u8> = Vec::new();
    f.extend_from_slice(b"APETAGEX");
    f.extend_from_slice(&2000u32.to_le_bytes());     // version
    f.extend_from_slice(&(footer_size as u32).to_le_bytes()); // size
    f.extend_from_slice(&(items.len() as u32).to_le_bytes()); // item count
    f.extend_from_slice(&0u32.to_le_bytes());        // flags: 无 header
    f.extend_from_slice(&[0u8; 8]);                  // reserved（footer 共 32B）
    let mut out = vec![0xFF, 0xFB, 0x90, 0x00];      // 假音频头占位
    out.extend_from_slice(&body);
    out.extend_from_slice(&f);
    if with_id3v1_tail { out.extend_from_slice(&vec![b'U'; 128]) } // ID3v1 垃圾尾之后
    out
}

#[test]
fn ape_footer_at_eof_parsed_with_lowercase_keys() {
    let buf = build_ape(&[("Title", "华夏传说"), ("ARTIST", "凤凰传奇")], false);
    let tag = parse_ape_tag(&buf).expect("应识别出 APEv2");
    assert_eq!(tag.items.len(), 2);
    // ⚠️ 关键契约：APEv2 键大小写不敏感，解析时统一小写——下游按 'title' 取值依赖这点。
    //   （我的第一版移植漏了 to_lowercase，靠这个断言才能抓住）
    assert_eq!(tag.items[0].key, "title", "键必须小写化");
    assert_eq!(tag.items[1].key, "artist");
    assert_eq!(tag.items[0].value, "华夏传说".as_bytes());
    assert_eq!(tag.version, 2000);
}

#[test]
fn ape_footer_before_id3v1_tail_is_found() {
    // 经典 MP3 布局：音频 + APEv2 + ID3v1（尾部 128B）→ footer 在 len-160
    let buf = build_ape(&[("title", "盛夏")], true);
    let tag = parse_ape_tag(&buf).expect("ID3v1 之前的 APE footer 也要能找到");
    assert_eq!(tag.items[0].key, "title");
    assert_eq!(tag.items[0].value, "盛夏".as_bytes());
    let at = find_ape_footer(&buf).expect("footer 偏移");
    assert_eq!(&buf[at..at + 8], b"APETAGEX");
}

#[test]
fn ape_absent_returns_none_not_fake_tag() {
    // 全 0x55 垃圾尾：不得凭空造出 tag（否则 readMp3Tags 会走错兜底分支）
    let junk: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    assert!(parse_ape_tag(&junk).is_none());
    assert!(find_ape_footer(&junk).is_none());
}

#[test]
fn ape_zero_value_item_does_not_hang() {
    // 零长值且 key 抵达区域末尾：p 无法前进 → 缺显式跳出就是死循环。
    // 用子进程超时来锁（cargo test 自身卡住的话这里永远不返回）。
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(&0u32.to_le_bytes());   // valueSize = 0
    body.extend_from_slice(&0u32.to_le_bytes());   // flags
    body.extend_from_slice(b"x"); body.push(0);    // key="x\0" 正好贴着末尾
    let mut f: Vec<u8> = Vec::new();
    f.extend_from_slice(b"APETAGEX");
    f.extend_from_slice(&2000u32.to_le_bytes());
    f.extend_from_slice(&(body.len() as u32).to_le_bytes());
    f.extend_from_slice(&1u32.to_le_bytes());
    f.extend_from_slice(&0u32.to_le_bytes());
    f.extend_from_slice(&[0u8; 8]);
    let mut buf = vec![0xFF, 0xFB, 0x90, 0x00];
    buf.extend_from_slice(&body); buf.extend_from_slice(&f);
    let tag = parse_ape_tag(&buf).expect("畸形但可容忍：应返回 tag 而非 panic/hang");
    assert_eq!(tag.items.len(), 1);
    assert_eq!(tag.items[0].key, "x");
    assert!(tag.items[0].value.is_empty());
}

#[test]
fn ape_truncated_region_clamps_value_end() {
    // valueSize 谎报得比实际大：valueEnd 必须夹到 region 末尾，不能越界 panic
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(&9999u32.to_le_bytes()); // 虚高的 valueSize
    body.extend_from_slice(&0u32.to_le_bytes());
    body.extend_from_slice(b"title"); body.push(0);
    body.extend_from_slice("短".as_bytes());
    let mut f: Vec<u8> = Vec::new();
    f.extend_from_slice(b"APETAGEX");
    f.extend_from_slice(&2000u32.to_le_bytes());
    f.extend_from_slice(&(body.len() as u32).to_le_bytes());
    f.extend_from_slice(&1u32.to_le_bytes());
    f.extend_from_slice(&0u32.to_le_bytes());
    f.extend_from_slice(&[0u8; 8]);
    let mut buf = vec![0xFF, 0xFB, 0x90, 0x00];
    buf.extend_from_slice(&body); buf.extend_from_slice(&f);
    let tag = parse_ape_tag(&buf).expect("越界应被夹住而不是 panic");
    assert_eq!(tag.items[0].key, "title");
    assert_eq!(tag.items[0].value, "短".as_bytes(), "值应截到区域末尾");
}
