use agentdrive_model::*;
use similar::{ChangeTag, TextDiff};
use std::time::Duration;

pub(crate) const MAX_BYTES: usize = 256 * 1024;
const MAX_LINES: usize = 10_000;

pub(crate) fn compare(
    mut result: RevisionTextDiff,
    before: &[u8],
    after: &[u8],
) -> RevisionTextDiff {
    let (Ok(before), Ok(after)) = (std::str::from_utf8(before), std::str::from_utf8(after)) else {
        result.status = TextDiffStatus::Binary;
        return result;
    };
    if before.contains('\0') || after.contains('\0') {
        result.status = TextDiffStatus::Binary;
        return result;
    }
    if before.len() > MAX_BYTES
        || after.len() > MAX_BYTES
        || before.lines().count() > MAX_LINES
        || after.lines().count() > MAX_LINES
    {
        result.status = TextDiffStatus::TooLarge;
        return result;
    }
    if before == after {
        result.status = TextDiffStatus::Unchanged;
        return result;
    }
    let diff = TextDiff::configure()
        .timeout(Duration::from_millis(200))
        .diff_lines(before, after);
    for group in diff.grouped_ops(3) {
        result.hunks.push(
            group
                .iter()
                .flat_map(|op| diff.iter_changes(op))
                .map(|change| DiffLine {
                    kind: match change.tag() {
                        ChangeTag::Equal => DiffLineKind::Context,
                        ChangeTag::Insert => DiffLineKind::Added,
                        ChangeTag::Delete => DiffLineKind::Removed,
                    },
                    before_line: change.old_index().map(|i| i + 1),
                    after_line: change.new_index().map(|i| i + 1),
                    text: change.value().trim_end_matches('\n').to_owned(),
                    newline: !change.missing_newline(),
                })
                .collect(),
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff(before: &[u8], after: &[u8]) -> RevisionTextDiff {
        compare(
            RevisionTextDiff {
                status: TextDiffStatus::Text,
                before: None,
                after: None,
                hunks: Vec::new(),
            },
            before,
            after,
        )
    }

    #[test]
    fn text_limits_binary_and_line_endings_are_explicit() {
        assert_eq!(diff(b"", &[0xff]).status, TextDiffStatus::Binary);
        assert_eq!(diff(b"", b"\0").status, TextDiffStatus::Binary);
        assert_eq!(
            diff(b"", "x\n".repeat(MAX_LINES + 1).as_bytes()).status,
            TextDiffStatus::TooLarge
        );
        assert_eq!(
            diff(b"", &vec![b'x'; MAX_BYTES + 1]).status,
            TextDiffStatus::TooLarge
        );
        let changes = diff(b"context\r\nold\r\n", b"context\r\nnew");
        let lines = &changes.hunks[0];
        assert_eq!(lines[0].text, "context\r");
        assert_eq!(lines[0].before_line, Some(1));
        assert_eq!(lines[0].after_line, Some(1));
        assert!(lines[0].newline);
        assert_eq!(lines[2].text, "new");
        assert_eq!(lines[2].after_line, Some(2));
        assert!(!lines[2].newline);
    }
}
