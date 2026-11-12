//! The stored patch's lines, with each side's real coordinates. No working tree reads.
use anyhow::{Result, bail, ensure};

#[derive(Debug, Clone)]
pub(crate) struct File {
    pub path: String,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone)]
pub(crate) struct Row {
    pub text: String,
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub kind: char,
    pub hunk: usize,
}

pub(crate) fn parse(patch: &str) -> Result<Vec<File>> {
    let mut files = Vec::new();
    let mut file: Option<File> = None;
    let (mut old, mut new, mut old_left, mut new_left) = (0, 0, 0, 0);
    let mut hunk = 0;
    let mut in_hunk = false;
    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            ensure!(old_left == 0 && new_left == 0, "truncated diff hunk");
            if let Some(file) = file.take() {
                files.push(file);
            }
            file = Some(File { path: header_path(line)?, rows: Vec::new() });
            in_hunk = false;
            hunk = 0;
            continue;
        }
        // Traditional unified diffs have no diff --git header. A new old-side marker
        // after a complete hunk starts another file, never a code line in that hunk.
        if line.starts_with("--- ") && old_left == 0 && new_left == 0 && (file.is_none() || in_hunk) {
            if let Some(previous) = file.take() {
                files.push(previous);
            }
            file = Some(File { path: marker_path(line.get(4..).unwrap_or_default())?, rows: Vec::new() });
            in_hunk = false;
            hunk = 0;
        }
        let Some(current) = file.as_mut() else { continue };
        if !in_hunk && line.starts_with("+++ ") {
            let filename = marker_path(line.get(4..).unwrap_or_default())?;
            if filename != "/dev/null" {
                current.path = filename;
            }
        }
        if let Some(header) = line.strip_prefix("@@ ") {
            ensure!(old_left == 0 && new_left == 0, "truncated diff hunk in {}", current.path);
            let mut parts = header.split_whitespace();
            let before = parts.next().unwrap_or_default();
            let after = parts.next().unwrap_or_default();
            ensure!(parts.next() == Some("@@"), "invalid diff hunk in {}", current.path);
            (old, old_left) = range(before, '-')?;
            (new, new_left) = range(after, '+')?;
            hunk += 1;
            in_hunk = true;
            current.rows.push(Row { text: line.into(), old: None, new: None, kind: '@', hunk });
            continue;
        }
        let kind = line.chars().next().unwrap_or(' ');
        let mut row = Row { text: line.into(), old: None, new: None, kind: ' ', hunk };
        if in_hunk && (old_left > 0 || new_left > 0) {
            match kind {
                ' ' | '-' | '+' => {
                    row.text = line.get(1..).unwrap_or_default().into();
                    row.kind = kind;
                    if kind != '+' {
                        ensure!(old_left > 0, "invalid old-side hunk length in {}", current.path);
                        row.old = Some(old);
                        old += 1;
                        old_left -= 1;
                    }
                    if kind != '-' {
                        ensure!(new_left > 0, "invalid new-side hunk length in {}", current.path);
                        row.new = Some(new);
                        new += 1;
                        new_left -= 1;
                    }
                }
                '\\' => {}
                _ => bail!("invalid diff hunk row in {}", current.path),
            }
        }
        current.rows.push(row);
    }
    ensure!(old_left == 0 && new_left == 0, "truncated diff hunk");
    if let Some(file) = file {
        files.push(file);
    }
    Ok(files)
}

fn range(word: &str, sign: char) -> Result<(usize, usize)> {
    let word = word.strip_prefix(sign).ok_or_else(|| anyhow::anyhow!("invalid diff hunk coordinate"))?;
    let (start, count) = word.split_once(',').unwrap_or((word, "1"));
    Ok((start.parse()?, count.parse()?))
}

fn header_path(line: &str) -> Result<String> {
    let rest = line.strip_prefix("diff --git ").unwrap_or_default();
    if rest.starts_with('"') {
        let (_, end) = quoted(rest)?;
        let new = rest.get(end..).unwrap_or_default().trim_start();
        return marker_path(new);
    }
    // Unquoted Git paths may contain spaces. The b/ separator is not a whitespace tokenizer.
    let (_, new) = rest.rsplit_once(" b/").ok_or_else(|| anyhow::anyhow!("invalid diff file header"))?;
    Ok(new.into())
}

fn marker_path(value: &str) -> Result<String> {
    let decoded = if value.starts_with('"') {
        quoted(value)?.0
    } else {
        value.split('\t').next().unwrap_or_default().into()
    };
    Ok(decoded.strip_prefix("a/").or_else(|| decoded.strip_prefix("b/")).unwrap_or(&decoded).into())
}

fn quoted(value: &str) -> Result<(String, usize)> {
    let mut bytes = Vec::new();
    let mut source = value.bytes().enumerate().skip(1);
    while let Some((at, byte)) = source.next() {
        match byte {
            b'"' => return Ok((String::from_utf8(bytes)?, at + 1)),
            b'\\' => {
                let (_, escape) =
                    source.next().ok_or_else(|| anyhow::anyhow!("unterminated Git path escape"))?;
                if (b'0'..=b'7').contains(&escape) {
                    let mut number = u16::from(escape - b'0');
                    for _ in 0..2 {
                        let (_, digit) =
                            source.next().ok_or_else(|| anyhow::anyhow!("short Git octal escape"))?;
                        ensure!((b'0'..=b'7').contains(&digit), "invalid Git octal escape");
                        number = number * 8 + u16::from(digit - b'0');
                    }
                    bytes.push(u8::try_from(number)?);
                } else {
                    bytes.push(match escape {
                        b't' => b'\t',
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b'a' => 7,
                        b'b' => 8,
                        b'f' => 12,
                        b'v' => 11,
                        other => other,
                    });
                }
            }
            other => bytes.push(other),
        }
    }
    bail!("unterminated quoted Git path")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numbers_each_side_and_keeps_deletion_filename() -> Result<()> {
        let files =
            parse("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ /dev/null\n@@ -8,2 +7,0 @@\n-old\n-lines\n")?;
        assert_eq!(files.first().map(|f| f.path.as_str()), Some("a.rs"));
        let row =
            files.first().and_then(|file| file.rows.last()).ok_or_else(|| anyhow::anyhow!("missing row"))?;
        assert_eq!((row.old, row.new), (Some(9), None));
        Ok(())
    }
    #[test]
    fn decodes_git_octal_utf8_paths_and_spaces() -> Result<()> {
        assert_eq!(header_path("diff --git \"a/\\303\\251 file\" \"b/\\303\\251 file\"")?, "é file");
        assert_eq!(header_path("diff --git a/a file.rs b/a file.rs")?, "a file.rs");
        Ok(())
    }
    #[test]
    fn refuses_incomplete_hunks_instead_of_inventing_line_numbers() {
        assert!(parse("diff --git a/x b/x\n@@ -1,2 +1,2 @@\n only one\n").is_err());
    }
}
