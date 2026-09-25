//! APEv2 tag 解析 —— 移植自 src/tag/read/apev2.ts
//! footer 定位：文件末尾，或 ID3v1（尾部 128B）之前。
//! item 布局：valueSize(4 LE) + flags(4 LE) + key(\0 结尾) + value
use super::id3v2::{latin1_decode, le_u32_at};

const MAGIC: &[u8; 8] = b"APETAGEX";

#[derive(Debug, Clone)]
pub struct ApeItem { pub key: String, pub value: Vec<u8> }

#[derive(Debug, Clone)]
pub struct ApeTag { pub version: u32, pub items: Vec<ApeItem>, pub flags: u32, pub tag_size: u32 }

/// items_count 按规范读出但**不用于驱动遍历**（TS 同款：以 `while p+8<=len` 为准，
/// 因为畸形文件的声明值常与实际不符）。保留字段以便将来做一致性告警。
#[allow(dead_code)]
struct Footer { size: usize, version: u32, flags: u32, items: u32 }

/// 校验 pos 处是否为合法 APEv2 footer。size<=0 或越过文件头 → 非法。
fn try_footer(buf: &[u8], pos: usize) -> Option<Footer> {
    if pos.checked_add(32)? > buf.len() { return None }
    if &buf[pos..pos + 8] != MAGIC { return None }
    let size = le_u32_at(buf, pos + 12) as usize;
    let items = le_u32_at(buf, pos + 16);
    let flags = le_u32_at(buf, pos + 20);
    let version = le_u32_at(buf, pos + 8);
    if size <= 0 || pos < size { return None }
    Some(Footer { size, version, flags, items })
}

/// 遍历 item 区。**逐行对照 TS parseItems**：key 一律小写化（APEv2 键大小写不敏感，
/// 下游按 'title'/'artist' 取值依赖这点），且保留两处逃逸条件。
fn parse_items(region: &[u8]) -> Vec<ApeItem> {
    let mut items = Vec::new();
    let mut p = 0usize;
    while p + 8 <= region.len() {
        let value_size = le_u32_at(region, p) as usize;
        let mut key_end = p + 8;
        while key_end < region.len() && region[key_end] != 0 { key_end += 1 }
        if key_end >= region.len() { break }
        let key = latin1_decode(&region[p + 8..key_end]).to_lowercase();
        let value_start = key_end + 1;
        let mut value_end = value_start + value_size;
        if value_end > region.len() { value_end = region.len() }
        if value_start > region.len() { break }
        items.push(ApeItem { key, value: region[value_start..value_end].to_vec() });
        p = value_start + value_size;
        // 零长值且 key 已到末尾：p 不再前进，必须显式跳出（否则死循环）
        if value_size == 0 && key_end + 1 == region.len() { break }
    }
    items
}

/// 在 MP3 尾部区域解析 APEv2 tag；找不到返回 None。
pub fn parse_ape_tag(buf: &[u8]) -> Option<ApeTag> {
    // 两个候选位置都要尝试：短文件时第二个候选会下溢，必须跳过而非中止整函数。
    // （第一版移植误用 `?`，导致 len<160 的输入直接返回 None——TS 用的是 continue。）
    let cands = [Some(buf.len().saturating_sub(32)), buf.len().checked_sub(128 + 32)];
    for cand in cands.into_iter().flatten() {
        let pos = cand;
        let info = match try_footer(buf, pos) { Some(x) => x, None => continue };
        let region = &buf[pos - info.size..pos];
        return Some(ApeTag {
            version: info.version, items: parse_items(region),
            flags: info.flags, tag_size: info.size as u32,
        });
    }
    None
}

/// 是否在文件末尾（ID3v1 之前）存在 APEv2 footer，返回其偏移；无则 None。
pub fn find_ape_footer(buf: &[u8]) -> Option<usize> {
    let cands = [Some(buf.len().saturating_sub(32)), buf.len().checked_sub(128 + 32)];
    for pos in cands.into_iter().flatten() {
        if buf.len() >= pos + 32 && &buf[pos..pos + 8] == MAGIC { return Some(pos) }
    }
    None
}
