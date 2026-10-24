//! 条目级三方合并。
//!
//! 以远端原文为底稿：只删除本地删掉的条目、插入本地新增的条目；未被触碰的行
//! （包括 CRLF、块间空行、文件末尾缺少的换行）都按原字节保留。这样避开
//! `split_entries → render_file` 往返造成的格式漂移——那种漂移会让每次同步都
//! 误判成冲突。
//!
//! 无法安全合并时返回 `None`，由调用方生成冲突副本。

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::notes::{parse_block, split_spans, BlockSpan};

/// 条目身份：同一时间、同一正文、同一来源视为同一条笔记。
type Identity = (String, String, Option<String>);

pub(crate) struct MergeResult {
    pub text: String,
    /// 双方各自修改了同一时间的条目；两个版本都会保留，冲突数 +1。
    pub conflicts: usize,
}

/// 三方合并。任一份文件里存在无法解析的条目块时返回 `None`。
pub(crate) fn merge(base: &str, local: &str, remote: &str) -> Option<MergeResult> {
    let base = Parsed::new(base)?;
    let local = Parsed::new(local)?;
    let remote = Parsed::new(remote)?;
    Some(merge_parsed(&base, &local, &remote))
}

struct Parsed<'a> {
    header: Vec<String>,
    spans: Vec<BlockSpan>,
    entries: Vec<Identity>,
    /// 按 `\n` 切开的原始行，保留行尾换行符。
    raw: Vec<&'a str>,
    eol: &'static str,
}

impl<'a> Parsed<'a> {
    fn new(text: &'a str) -> Option<Self> {
        let layout = split_spans(text);
        let mut entries = Vec::with_capacity(layout.spans.len());
        for span in &layout.spans {
            entries.push(parse_block(&span.entry)?);
        }
        let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
        Some(Self {
            header: layout.header,
            spans: layout.spans,
            entries,
            raw: raw_lines(text),
            eol,
        })
    }
}

fn raw_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            lines.push(&text[start..=index]);
            start = index + 1;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

fn counts(entries: &[Identity]) -> HashMap<Identity, usize> {
    let mut map = HashMap::new();
    for entry in entries {
        *map.entry(entry.clone()).or_insert(0) += 1;
    }
    map
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

fn merge_parsed(base: &Parsed, local: &Parsed, remote: &Parsed) -> MergeResult {
    let base_counts = counts(&base.entries);
    let local_counts = counts(&local.entries);
    let remote_counts = counts(&remote.entries);

    // base 里被某一侧删掉的身份（按多重集计）。
    let mut removed_by_local: HashMap<Identity, usize> = HashMap::new();
    let mut removed_by_remote: HashMap<Identity, usize> = HashMap::new();
    for (identity, &base_count) in &base_counts {
        let local_count = local_counts.get(identity).copied().unwrap_or(0);
        if base_count > local_count {
            removed_by_local.insert(identity.clone(), base_count - local_count);
        }
        let remote_count = remote_counts.get(identity).copied().unwrap_or(0);
        if base_count > remote_count {
            removed_by_remote.insert(identity.clone(), base_count - remote_count);
        }
    }

    // 本地新增：同一身份在本地的出现次数超过 base 计数的那些，按文件顺序。
    let mut local_added: Vec<usize> = Vec::new();
    let mut seen: HashMap<&Identity, usize> = HashMap::new();
    for (index, identity) in local.entries.iter().enumerate() {
        let count = seen.entry(identity).or_insert(0);
        *count += 1;
        if *count > base_counts.get(identity).copied().unwrap_or(0) {
            local_added.push(index);
        }
    }

    // 远端要删的块：本地相对 base 少掉的条目，扣掉远端自己也已经删掉的部分。
    // 否则双方各删一份重复条目时，会把对方保留的最后一份也删掉。
    let mut pending: HashMap<Identity, usize> = removed_by_local
        .iter()
        .filter_map(|(identity, &count)| {
            let already_removed = removed_by_remote.get(identity).copied().unwrap_or(0);
            let left = count.saturating_sub(already_removed);
            (left > 0).then(|| (identity.clone(), left))
        })
        .collect();
    let mut remove = vec![false; remote.spans.len()];
    for (index, identity) in remote.entries.iter().enumerate() {
        if let Some(left) = pending.get_mut(identity) {
            if *left > 0 {
                *left -= 1;
                remove[index] = true;
            }
        }
    }

    let mut remote_left = remote_counts;
    for (index, identity) in remote.entries.iter().enumerate() {
        if remove[index] {
            if let Some(left) = remote_left.get_mut(identity) {
                *left -= 1;
            }
        }
    }

    // 双方新增了相同条目时只保留一份：先算每个身份还差几份，再按本地顺序分配。
    let mut insert_left: HashMap<Identity, usize> = HashMap::new();
    let mut checked: HashSet<&Identity> = HashSet::new();
    for &index in &local_added {
        let identity = &local.entries[index];
        if !checked.insert(identity) {
            continue;
        }
        let base_count = base_counts.get(identity).copied().unwrap_or(0);
        let extra_local = local_counts
            .get(identity)
            .copied()
            .unwrap_or(0)
            .saturating_sub(base_count);
        let extra_remote = remote_left
            .get(identity)
            .copied()
            .unwrap_or(0)
            .saturating_sub(base_count);
        let needed = extra_local.saturating_sub(extra_remote);
        if needed > 0 {
            insert_left.insert(identity.clone(), needed);
        }
    }

    let mut additions: Vec<usize> = Vec::new();
    for &index in &local_added {
        if let Some(left) = insert_left.get_mut(&local.entries[index]) {
            if *left > 0 {
                *left -= 1;
                additions.push(index);
            }
        }
    }

    let conflicts = count_conflicts(
        local,
        remote,
        &base_counts,
        &removed_by_local,
        &removed_by_remote,
    );

    let text = render(base, local, remote, &remove, &additions);
    MergeResult { text, conflicts }
}

/// 双方都改写了同一个 base 条目、且新内容不同，才算一次冲突。
fn count_conflicts(
    local: &Parsed,
    remote: &Parsed,
    base_counts: &HashMap<Identity, usize>,
    removed_by_local: &HashMap<Identity, usize>,
    removed_by_remote: &HashMap<Identity, usize>,
) -> usize {
    let mut counted: HashSet<&str> = HashSet::new();
    let mut conflicts = 0;
    for (identity, &removed) in removed_by_local {
        let both = removed.min(removed_by_remote.get(identity).copied().unwrap_or(0));
        if both == 0 {
            continue;
        }
        let time = identity.0.as_str();
        if !counted.insert(time) {
            continue;
        }
        let local_new = additions_at(local, base_counts, time);
        let remote_new = additions_at(remote, base_counts, time);
        if !local_new.is_empty() && !remote_new.is_empty() && local_new != remote_new {
            conflicts += 1;
        }
    }
    conflicts
}

/// 某一侧在指定时间点新增（相对 base）的条目，排序后便于比较。
fn additions_at<'a>(
    parsed: &'a Parsed,
    base_counts: &HashMap<Identity, usize>,
    time: &str,
) -> Vec<&'a Identity> {
    let mut seen: HashMap<&Identity, usize> = HashMap::new();
    let mut out = Vec::new();
    for identity in parsed.entries.iter().filter(|identity| identity.0 == time) {
        let count = seen.entry(identity).or_insert(0);
        *count += 1;
        if *count > base_counts.get(identity).copied().unwrap_or(0) {
            out.push(identity);
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.2.cmp(&b.2)));
    out
}

fn render(
    base: &Parsed,
    local: &Parsed,
    remote: &Parsed,
    remove: &[bool],
    additions: &[usize],
) -> String {
    let body_exists = !remote.spans.is_empty() || !additions.is_empty();
    let header_end = remote
        .spans
        .first()
        .map(|span| span.start_line)
        .unwrap_or(remote.raw.len());

    let mut out = String::new();
    let mut last_blank = true;

    // 头部：本地改过就用本地，否则沿用远端原文。
    if local.header != base.header {
        for line in &local.header {
            out.push_str(line);
            out.push_str(remote.eol);
            last_blank = false;
        }
        if !local.header.is_empty() && body_exists {
            out.push_str(remote.eol);
            last_blank = true;
        }
    } else {
        for line in &remote.raw[..header_end] {
            out.push_str(line);
            last_blank = is_blank(line);
        }
    }

    // 被删掉的块连同其后的空行一起跳过：块前的空行仍作为与上一块的分隔。
    let mut skip = vec![false; remote.raw.len()];
    for (index, span) in remote.spans.iter().enumerate() {
        if !remove[index] {
            continue;
        }
        for skipped in skip.iter_mut().take(span.end_line).skip(span.start_line) {
            *skipped = true;
        }
        let mut line = span.end_line;
        while line < remote.raw.len() && is_blank(remote.raw[line]) {
            skip[line] = true;
            line += 1;
        }
    }

    // 新增块按时间插到第一个更晚的块之前；没有更晚的块就接在最后一块之后。
    let mut inserts: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut last_anchor = header_end;
    for &index in additions {
        let time = local.entries[index].0.as_str();
        let anchor = insertion_anchor(time, remote, remove, header_end).max(last_anchor);
        last_anchor = anchor;
        inserts.entry(anchor).or_default().push(index);
    }

    let mut line = header_end;
    while line < remote.raw.len() {
        if let Some(blocks) = inserts.remove(&line) {
            last_blank = emit_blocks(&mut out, local, remote, &blocks, line, &skip, last_blank);
        }
        if skip[line] {
            line += 1;
            continue;
        }
        out.push_str(remote.raw[line]);
        last_blank = is_blank(remote.raw[line]);
        line += 1;
    }
    if let Some(blocks) = inserts.remove(&remote.raw.len()) {
        emit_blocks(
            &mut out,
            local,
            remote,
            &blocks,
            remote.raw.len(),
            &skip,
            last_blank,
        );
    }

    out
}

fn insertion_anchor(time: &str, remote: &Parsed, remove: &[bool], header_end: usize) -> usize {
    for (index, span) in remote.spans.iter().enumerate() {
        if !remove[index] && remote.entries[index].0.as_str() > time {
            return span.start_line;
        }
    }
    for (index, span) in remote.spans.iter().enumerate().rev() {
        if !remove[index] {
            // 接在最后一块之后时，越过它原有的空行，让这些空行保持原有位置。
            let mut anchor = span.end_line;
            while anchor < remote.raw.len() && is_blank(remote.raw[anchor]) {
                anchor += 1;
            }
            return anchor;
        }
    }
    header_end
}

fn next_kept_non_blank(anchor: usize, remote: &Parsed, skip: &[bool]) -> bool {
    remote
        .raw
        .iter()
        .zip(skip)
        .skip(anchor)
        .find(|(_, skipped)| !**skipped)
        .is_some_and(|(line, _)| !is_blank(line))
}

/// 插入若干个本地新增块（保持本地顺序），返回插入后的「上一行是否为空行」。
fn emit_blocks(
    out: &mut String,
    local: &Parsed,
    remote: &Parsed,
    blocks: &[usize],
    anchor: usize,
    skip: &[bool],
    mut last_blank: bool,
) -> bool {
    for &index in blocks {
        if !last_blank {
            // 末行没有换行符时（文件不以换行结尾），先补一个再空行。
            if !out.ends_with('\n') {
                out.push_str(remote.eol);
            }
            out.push_str(remote.eol);
        }
        for line in &local.spans[index].entry.lines {
            out.push_str(line);
            out.push_str(remote.eol);
        }
        last_blank = false;
        if next_kept_non_blank(anchor, remote, skip) {
            out.push_str(remote.eol);
            last_blank = true;
        }
    }
    last_blank
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merged(base: &str, local: &str, remote: &str) -> MergeResult {
        merge(base, local, remote).expect("应当可以合并")
    }

    #[test]
    fn both_sides_append_in_time_order() {
        let base = "# 2026-10-23\n\n- 10:00 base\n";
        let local = "# 2026-10-23\n\n- 10:00 base\n\n- 11:00 local\n";
        let remote = "# 2026-10-23\n\n- 10:00 base\n\n- 12:00 remote\n";
        let result = merged(base, local, remote);
        assert_eq!(
            result.text,
            "# 2026-10-23\n\n- 10:00 base\n\n- 11:00 local\n\n- 12:00 remote\n"
        );
        assert_eq!(result.conflicts, 0);
    }

    #[test]
    fn both_sides_edit_same_entry_keeps_both() {
        let base = "# 2026-10-23\n\n- 10:00 old\n";
        let local = "# 2026-10-23\n\n- 10:00 local\n";
        let remote = "# 2026-10-23\n\n- 10:00 remote\n";
        let result = merged(base, local, remote);
        assert!(result.text.contains("- 10:00 remote\n"));
        assert!(result.text.contains("- 10:00 local\n"));
        assert!(!result.text.contains("old"));
        assert_eq!(result.conflicts, 1);
    }

    #[test]
    fn local_deletion_removes_remote_block() {
        let base = "# 2026-10-23\n\n- 10:00 a\n\n- 11:00 b\n";
        let local = "# 2026-10-23\n\n- 11:00 b\n";
        let remote = base;
        let result = merged(base, local, remote);
        assert_eq!(result.text, "# 2026-10-23\n\n- 11:00 b\n");
        assert_eq!(result.conflicts, 0);
    }

    #[test]
    fn local_delete_vs_remote_edit_keeps_remote_edit() {
        let base = "# 2026-10-23\n\n- 10:00 a\n";
        let local = "# 2026-10-23\n";
        let remote = "# 2026-10-23\n\n- 10:00 changed\n";
        let result = merged(base, local, remote);
        assert_eq!(result.text, remote);
        assert_eq!(result.conflicts, 0);
    }

    #[test]
    fn remote_delete_vs_local_edit_keeps_local_edit() {
        let base = "# 2026-10-23\n\n- 10:00 a\n";
        let local = "# 2026-10-23\n\n- 10:00 changed\n";
        let remote = "# 2026-10-23\n";
        let result = merged(base, local, remote);
        assert_eq!(result.text, "# 2026-10-23\n\n- 10:00 changed\n");
        assert_eq!(result.conflicts, 0);
    }

    #[test]
    fn duplicate_entries_only_delete_one_copy() {
        let base = "# 2026-10-23\n\n- 10:00 x\n\n- 10:00 x\n";
        let local = "# 2026-10-23\n\n- 10:00 x\n";
        let result = merged(base, local, base);
        assert_eq!(result.text, "# 2026-10-23\n\n- 10:00 x\n");
    }

    #[test]
    fn both_sides_deleting_one_duplicate_keeps_the_last_copy() {
        let base = "# 2026-10-23\n\n- 10:00 x\n\n- 10:00 x\n";
        let local = "# 2026-10-23\n\n- 10:00 x\n";
        let remote = "# 2026-10-23\n\n- 10:00 x\n";
        let result = merged(base, local, remote);
        assert_eq!(result.text, "# 2026-10-23\n\n- 10:00 x\n");
        assert_eq!(result.conflicts, 0);
    }

    #[test]
    fn identical_additions_are_deduplicated() {
        let base = "# 2026-10-23\n\n- 10:00 base\n";
        let local = "# 2026-10-23\n\n- 10:00 base\n\n- 11:00 same\n";
        let remote = "# 2026-10-23\n\n- 10:00 base\n\n- 11:00 same\n";
        let result = merged(base, local, remote);
        assert_eq!(
            result.text,
            "# 2026-10-23\n\n- 10:00 base\n\n- 11:00 same\n"
        );
        assert_eq!(result.conflicts, 0);
    }

    #[test]
    fn keeps_crlf_style() {
        let base = "# 2026-10-23\r\n\r\n- 10:00 base\r\n";
        let local = "# 2026-10-23\r\n\r\n- 10:00 base\r\n\r\n- 11:00 local\r\n";
        let remote = "# 2026-10-23\r\n\r\n- 10:00 base\r\n\r\n- 12:00 remote\r\n";
        let result = merged(base, local, remote);
        assert_eq!(
            result.text,
            "# 2026-10-23\r\n\r\n- 10:00 base\r\n\r\n- 11:00 local\r\n\r\n- 12:00 remote\r\n"
        );
        assert!(!result.text.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn preserves_multiple_blank_lines() {
        let base = "# 2026-10-23\n\n\n- 10:00 a\n\n\n";
        let local = "# 2026-10-23\n\n\n- 10:00 a\n\n\n- 11:00 b\n";
        let result = merged(base, local, base);
        assert_eq!(result.text, "# 2026-10-23\n\n\n- 10:00 a\n\n\n- 11:00 b\n");
    }

    #[test]
    fn handles_missing_trailing_newline() {
        let base = "# 2026-10-23\n\n- 10:00 a";
        let local = "# 2026-10-23\n\n- 10:00 a\n\n- 11:00 b";
        let remote = "# 2026-10-23\n\n- 10:00 a";
        let result = merged(base, local, remote);
        assert!(result.text.ends_with("- 11:00 b\n"));
        assert!(result.text.contains("- 10:00 a\n\n- 11:00 b"));
    }

    #[test]
    fn keeps_markdown_block_formatting() {
        let base = "# 2026-10-23\n\n### 10:00\n正文\n";
        let local = "# 2026-10-23\n\n### 10:00\n正文\n\n- 11:00 新增\n";
        let remote = "# 2026-10-23\n\n### 10:00\n正文\n\n### 12:00\n远端\n";
        let result = merged(base, local, remote);
        assert_eq!(
            result.text,
            "# 2026-10-23\n\n### 10:00\n正文\n\n- 11:00 新增\n\n### 12:00\n远端\n"
        );
    }

    #[test]
    fn local_header_wins_when_changed() {
        let base = "# 2026-10-23\n\n- 10:00 a\n";
        let local = "# 2026-10-23 周五\n\n- 10:00 a\n";
        let remote = "# 2026-10-23\n\n- 10:00 a\n\n- 11:00 b\n";
        let result = merged(base, local, remote);
        assert!(result.text.starts_with("# 2026-10-23 周五\n\n- 10:00 a"));
        assert!(result.text.contains("- 11:00 b"));
    }

    #[test]
    fn unchanged_local_returns_remote_verbatim() {
        let base = "# 2026-10-23\n\n- 10:00 a\n";
        let remote = "# 2026-10-23\r\n\r\n- 10:00 a\r\n\r\n- 11:00 b\r\n";
        let result = merged(base, base, remote);
        assert_eq!(result.text, remote);
    }

    #[test]
    fn unparseable_block_falls_back() {
        let text = "# 2026-10-23\n\n- 10:00\n";
        assert!(merge(text, text, text).is_none());
    }

    #[test]
    fn empty_files_merge_to_empty() {
        let result = merged("", "", "");
        assert_eq!(result.text, "");
    }
}
