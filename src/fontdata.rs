//! TTC（TrueType Collection）首字重提取：纯 std，仅做字节结构搬运。
//!
//! 背景：界面把系统字体文件（首选 `C:\Windows\Fonts\msyh.ttc` 微软雅黑）
//! 读入后交给 egui 作中文回退字体。epaint 的 `FontData::from_owned`
//! 把 face 索引固定为 0（见 epaint 0.36 源码），即 TTC 里只有第一个
//! 字重会被真正使用，但整包字节（其余字重也在内）会随 FontData 一直
//! 常驻内存。本模块把第一个字重重建成独立的 TTF/OTF 字节再喂给 egui，
//! 常驻量只剩实际用到的那一个字重；字形数据逐字节复制、目录顺序与
//! 原字重一致，渲染结果按构造与原先等价。
//!
//! 容错：非 TTC、结构损坏、偏移越界一律返回 None，调用方回退为使用
//! 原始字节（与不做提取时完全相同），绝不 panic。

/// TTC 文件魔数。
const TTC_MAGIC: &[u8; 4] = b"ttcf";
/// 目录项数量的合理上限（真实字体远低于此值，仅用于挡损坏数据）。
const MAX_TABLES: usize = 512;

fn be_u16(data: &[u8], off: usize) -> Option<u16> {
    let b = data.get(off..off + 2)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

fn be_u32(data: &[u8], off: usize) -> Option<u32> {
    let b = data.get(off..off + 4)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// 是否为 TTC 集合文件。
pub fn is_ttc(data: &[u8]) -> bool {
    data.starts_with(TTC_MAGIC)
}

/// 一条 sfnt 表目录项（偏移/长度均针对源数据）。
struct TableRec {
    tag: [u8; 4],
    checksum: u32,
    offset: usize,
    length: usize,
}

/// 提取 TTC 的第一个字重，构造独立字体文件的字节。
///
/// 结构搬运规则（TTC 与 sfnt 的公开格式）：
/// - TTC 头：魔数 `ttcf` + 版本（4）+ 字重数（4）+ 各字重 sfnt 头偏移
///   数组；第 0 个偏移即首字重。
/// - 字重 sfnt 头：版本号（4）+ 表数（2）+ 检索参数（6），其后是
///   16 字节/条的表目录：标签、校验和、偏移（相对 TTC 文件头）、长度。
/// - 独立文件布局：同样的 12 字节头 + 目录 + 各表数据按原顺序拼接、
///   4 字节对齐补零，目录里的偏移改写为新位置；表数据逐字节复制，
///   校验和沿用原值——唯一例外是 `head` 表的 checkSumAdjustment 字段
///   （表内偏移 8 起 4 字节）：其目录校验和本来就是按该字段为 0 算的，
///   故复制时清零以保持自洽；字体解析器（ab_glyph）不校验此字段。
///
/// 非 TTC 或任何结构异常返回 None。
pub fn first_face(data: &[u8]) -> Option<Vec<u8>> {
    if !is_ttc(data) {
        return None;
    }
    let num_fonts = be_u32(data, 8)?;
    if num_fonts == 0 {
        return None;
    }
    let face_off = be_u32(data, 12)? as usize;

    // 字重 sfnt 头（12 字节）。
    let sfnt_version = be_u32(data, face_off)?;
    let num_tables = be_u16(data, face_off + 4)? as usize;
    if num_tables == 0 || num_tables > MAX_TABLES {
        return None;
    }
    // 头后 6 字节（numTables 已读，其余为检索参数）原样保留。
    let head_tail = data.get(face_off + 4..face_off + 12)?;

    // 解析表目录，逐项做边界校验。
    let dir_off = face_off.checked_add(12)?;
    let mut recs: Vec<TableRec> = Vec::with_capacity(num_tables);
    for i in 0..num_tables {
        let ro = dir_off.checked_add(i.checked_mul(16)?)?;
        let raw = data.get(ro..ro + 16)?;
        let tag = [raw[0], raw[1], raw[2], raw[3]];
        let checksum = be_u32(data, ro + 4)?;
        let offset = be_u32(data, ro + 8)? as usize;
        let length = be_u32(data, ro + 12)? as usize;
        // 表数据必须完整落在源文件内（checked 防溢出与越界）。
        offset.checked_add(length).filter(|&end| end <= data.len())?;
        recs.push(TableRec {
            tag,
            checksum,
            offset,
            length,
        });
    }

    // 组装独立文件：头 + 目录（先占位）+ 各表数据（4 字节对齐）。
    let header_len = 12 + num_tables * 16;
    let mut out: Vec<u8> = Vec::with_capacity(header_len + data.len() / 2);
    out.extend_from_slice(&sfnt_version.to_be_bytes());
    out.extend_from_slice(head_tail);
    out.resize(header_len, 0);

    let mut new_offsets: Vec<u32> = Vec::with_capacity(num_tables);
    for rec in &recs {
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        new_offsets.push(out.len() as u32);
        let start = out.len();
        out.extend_from_slice(&data[rec.offset..rec.offset + rec.length]);
        if &rec.tag == b"head" && rec.length >= 12 {
            // checkSumAdjustment 清零（见模块注释）。
            out[start + 8..start + 12].copy_from_slice(&[0, 0, 0, 0]);
        }
    }

    // 回填目录：标签与校验和沿用原值，偏移写新位置。
    for (i, rec) in recs.iter().enumerate() {
        let ro = 12 + i * 16;
        out[ro..ro + 4].copy_from_slice(&rec.tag);
        out[ro + 4..ro + 8].copy_from_slice(&rec.checksum.to_be_bytes());
        out[ro + 8..ro + 12].copy_from_slice(&new_offsets[i].to_be_bytes());
        out[ro + 12..ro + 16].copy_from_slice(&(rec.length as u32).to_be_bytes());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_u16(v: &mut Vec<u8>, n: u16) {
        v.extend_from_slice(&n.to_be_bytes());
    }
    fn put_u32(v: &mut Vec<u8>, n: u32) {
        v.extend_from_slice(&n.to_be_bytes());
    }

    /// 造一个最小的假字重：sfnt 头 + 两张表（head / glyf），
    /// 表数据紧跟目录之后，偏移按真实格式填写。返回 (字节, glyf 数据)。
    fn fake_face(glyf_payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        put_u32(&mut v, 0x0001_0000); // sfnt 版本
        put_u16(&mut v, 2); // 表数
        put_u16(&mut v, 32); // searchRange
        put_u16(&mut v, 1); // entrySelector
        put_u16(&mut v, 0); // rangeShift
        let head_off = 12 + 2 * 16;
        let glyf_off = head_off + 20;
        // head 表目录项
        v.extend_from_slice(b"head");
        put_u32(&mut v, 0xAAAA);
        put_u32(&mut v, head_off as u32);
        put_u32(&mut v, 20);
        // glyf 表目录项
        v.extend_from_slice(b"glyf");
        put_u32(&mut v, 0xBBBB);
        put_u32(&mut v, glyf_off as u32);
        put_u32(&mut v, glyf_payload.len() as u32);
        // head 表数据 20 字节：checkSumAdjustment（偏移 8）故意填非零。
        let mut head = vec![0u8; 20];
        head[8..12].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        v.extend_from_slice(&head);
        v.extend_from_slice(glyf_payload);
        v
    }

    /// 把字重内各表的偏移整体平移 `base`：真实 TTC 里表偏移是相对
    /// 整个文件头的绝对偏移，拼装夹具时必须把字重本地偏移加上它在
    /// 文件中的起始位置，与真实文件保持一致。
    fn shift_face_offsets(face: &mut [u8], base: u32) {
        let n = be_u16(face, 4).unwrap() as usize;
        for i in 0..n {
            let ro = 12 + i * 16 + 8;
            let old = be_u32(face, ro).unwrap();
            face[ro..ro + 4].copy_from_slice(&(old + base).to_be_bytes());
        }
    }

    /// 把两个假字重拼成 TTC：face0 在前，face1 在后。
    fn fake_ttc() -> (Vec<u8>, Vec<u8>) {
        let mut face0 = fake_face(b"FACE0-GLYF");
        let mut face1 = fake_face(b"FACE1-GLYF-LONGER");
        let face0_off = 12 + 2 * 4;
        let face1_off = face0_off + face0.len();
        shift_face_offsets(&mut face0, face0_off as u32);
        shift_face_offsets(&mut face1, face1_off as u32);
        let mut v = Vec::new();
        v.extend_from_slice(b"ttcf");
        put_u32(&mut v, 0x0001_0000); // TTC 版本
        put_u32(&mut v, 2); // 字重数
        put_u32(&mut v, face0_off as u32);
        put_u32(&mut v, face1_off as u32);
        v.extend_from_slice(&face0);
        v.extend_from_slice(&face1);
        (v, face0)
    }

    /// 从独立字体字节里取出某张表的数据（按新目录偏移）。
    fn table_data<'a>(font: &'a [u8], want: &[u8; 4]) -> Option<&'a [u8]> {
        let n = be_u16(font, 4)? as usize;
        for i in 0..n {
            let ro = 12 + i * 16;
            if &font[ro..ro + 4] == want {
                let off = be_u32(font, ro + 8)? as usize;
                let len = be_u32(font, ro + 12)? as usize;
                return font.get(off..off + len);
            }
        }
        None
    }

    #[test]
    fn extracts_first_face_tables_verbatim() {
        let (ttc, _face0) = fake_ttc();
        let out = first_face(&ttc).expect("应能提取首字重");
        // 不再是集合文件，版本号保留。
        assert!(!is_ttc(&out));
        assert_eq!(be_u32(&out, 0), Some(0x0001_0000));
        assert_eq!(be_u16(&out, 4), Some(2));
        // 取到是 face0 的 glyf，不是 face1 的。
        assert_eq!(table_data(&out, b"glyf"), Some(&b"FACE0-GLYF"[..]));
        // head 表在，checkSumAdjustment 已清零。
        let head = table_data(&out, b"head").expect("head 应在");
        assert_eq!(&head[8..12], &[0, 0, 0, 0]);
        // 输出自洽：所有目录偏移+长度都不越界，且明显小于整包。
        let n = be_u16(&out, 4).unwrap() as usize;
        for i in 0..n {
            let ro = 12 + i * 16;
            let off = be_u32(&out, ro + 8).unwrap() as usize;
            let len = be_u32(&out, ro + 12).unwrap() as usize;
            assert!(off + len <= out.len());
        }
        assert!(out.len() < ttc.len());
    }

    #[test]
    fn rejects_non_ttc_and_broken_inputs() {
        assert!(first_face(b"").is_none());
        assert!(first_face(b"\x00\x01\x00\x00plain ttf").is_none());
        assert!(first_face(b"OTTO....").is_none());
        // 魔数对但截断。
        assert!(first_face(b"ttcf").is_none());
        assert!(first_face(b"ttcf\x00\x01\x00\x00").is_none());
        // 字重数为 0。
        let mut v = Vec::new();
        v.extend_from_slice(b"ttcf");
        put_u32(&mut v, 0x0001_0000);
        put_u32(&mut v, 0);
        assert!(first_face(&v).is_none());
        // 字重偏移越界。
        let mut v = Vec::new();
        v.extend_from_slice(b"ttcf");
        put_u32(&mut v, 0x0001_0000);
        put_u32(&mut v, 1);
        put_u32(&mut v, 999_999);
        assert!(first_face(&v).is_none());
        // 表数据越界：把 face0 的 glyf 长度改成天文数字。
        let (mut ttc, _) = fake_ttc();
        let face0_off = be_u32(&ttc, 12).unwrap() as usize;
        let glyf_len_pos = face0_off + 12 + 16 + 12;
        ttc[glyf_len_pos..glyf_len_pos + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(first_face(&ttc).is_none());
    }

    #[test]
    fn single_face_ttc_also_extracts() {
        // 只有一个字重的 TTC 同样应能提取（结果与该字重等价）。
        let mut face = fake_face(b"ONLY");
        shift_face_offsets(&mut face, 16);
        let mut v = Vec::new();
        v.extend_from_slice(b"ttcf");
        put_u32(&mut v, 0x0002_0000);
        put_u32(&mut v, 1);
        put_u32(&mut v, 16);
        v.extend_from_slice(&face);
        let out = first_face(&v).expect("单字重 TTC 应能提取");
        assert_eq!(table_data(&out, b"glyf"), Some(&b"ONLY"[..]));
    }
}
