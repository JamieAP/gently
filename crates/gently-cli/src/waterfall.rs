//! Terminal waterfall rendering for collector span rows.

use crate::query_client::SpanRow;
use anyhow::{ensure, Context, Result};
use std::collections::HashMap;
use std::fmt::Write;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const WIDTH: usize = 56;
const LABEL_WIDTH: usize = 26;
const LABEL_INPUT_CHARS: usize = 256;
const NESTING_SLACK: i128 = 2_000_000;

struct Bounds {
    raw_start: u64,
    raw_end: Option<u64>,
    start: u64,
    end: Option<u64>,
}

impl Bounds {
    fn end_or_start(&self) -> u64 {
        self.end.unwrap_or(self.start)
    }
}

fn timestamp(value: &str, field: &str, row: usize) -> Result<u64> {
    value
        .parse()
        .with_context(|| format!("invalid {field} nanosecond timestamp at span {}", row + 1))
}

fn optional_timestamp(value: Option<&str>, field: &str, row: usize) -> Result<Option<u64>> {
    value
        .filter(|value| !value.is_empty())
        .map(|value| timestamp(value, field, row))
        .transpose()
}

// Keep Python's round-to-even behavior, without losing nanoseconds by converting
// epoch timestamps to floating point. Even u64::MAX * WIDTH fits in i128.
fn columns(duration: i128, total: i128) -> i128 {
    let numerator = duration.max(0) * WIDTH as i128;
    let whole = numerator / total;
    let remainder = numerator % total;
    whole + i128::from(remainder * 2 > total || (remainder * 2 == total && whole % 2 == 1))
}

fn safe_char(value: char) -> char {
    if value.is_control()
        || matches!(value, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    {
        ' '
    } else {
        value
    }
}

fn label(name: &str, depth: usize) -> String {
    // Leave room for the name at great depths; never allocate a depth-sized
    // indentation string or clone a potentially very large span name.
    let indent = depth.saturating_mul(2).min(LABEL_WIDTH - 2);
    let mut label = " ".repeat(indent);
    // Bound work before grapheme segmentation: one cluster can contain an
    // arbitrarily large number of combining marks. If this prefix is cut short,
    // omit its final cluster, which could continue beyond the inspected input.
    let mut source = name.chars();
    let safe_prefix: String = source
        .by_ref()
        .take(LABEL_INPUT_CHARS)
        .map(safe_char)
        .collect();
    let truncated = source.next().is_some();
    let mut graphemes: Vec<_> = safe_prefix.graphemes(true).collect();
    if truncated {
        graphemes.pop();
    }
    let available = LABEL_WIDTH - indent;
    let name_width: usize = graphemes
        .iter()
        .map(|grapheme| UnicodeWidthStr::width(*grapheme))
        .sum();
    let clipped = truncated || name_width > available;
    let name_budget = available - usize::from(clipped);
    let mut width = indent;
    for grapheme in graphemes {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width - indent + grapheme_width > name_budget {
            break;
        }
        label.push_str(grapheme);
        width += grapheme_width;
    }
    if clipped {
        label.push('…');
        width += 1;
    }
    label.extend(std::iter::repeat_n(' ', LABEL_WIDTH - width));
    label
}

pub(crate) fn render(rows: &[SpanRow]) -> Result<String> {
    ensure!(!rows.is_empty(), "no spans");
    let mut by_id = HashMap::with_capacity(rows.len());
    let mut bounds = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        ensure!(
            by_id.insert(row.span_id.as_str(), index).is_none(),
            "duplicate span id at span {}",
            index + 1
        );
        let raw_start = timestamp(&row.start_unix_nano, "start_unix_nano", index)?;
        let raw_end = optional_timestamp(row.end_unix_nano.as_deref(), "end_unix_nano", index)?;
        let start = optional_timestamp(
            row.effective_start_unix_nano.as_deref(),
            "effective_start_unix_nano",
            index,
        )?
        .unwrap_or(raw_start);
        let end = optional_timestamp(
            row.effective_end_unix_nano.as_deref(),
            "effective_end_unix_nano",
            index,
        )?
        .or(raw_end);
        bounds.push(Bounds {
            raw_start,
            raw_end,
            start,
            end,
        });
    }

    let mut children = vec![Vec::new(); rows.len()];
    let mut parents = Vec::with_capacity(rows.len());
    let mut roots = Vec::new();
    let mut nonroot = 0;
    let mut resolved = 0;
    let mut session_roots = 0;
    for (index, row) in rows.iter().enumerate() {
        let parent_id = row.parent_span_id.as_deref().filter(|id| !id.is_empty());
        let parent = parent_id.and_then(|id| by_id.get(id).copied());
        if parent_id.is_some() {
            nonroot += 1;
        } else if row.name == "session" {
            session_roots += 1;
        }
        if let Some(parent) = parent {
            children[parent].push(index);
            resolved += 1;
        } else {
            roots.push(index);
        }
        parents.push(parent);
    }
    roots.sort_by_key(|&index| bounds[index].raw_start);
    for siblings in &mut children {
        siblings.sort_by_key(|&index| bounds[index].raw_start);
    }

    // Each span has at most one parent. An iterative walk from all resolved or
    // dangling roots visits every acyclic component exactly once; any unvisited
    // component necessarily contains a parent cycle.
    let mut stack: Vec<_> = roots
        .into_iter()
        .rev()
        .map(|index| (index, 0usize))
        .collect();
    let mut order = Vec::with_capacity(rows.len());
    while let Some((index, depth)) = stack.pop() {
        order.push((index, depth));
        stack.extend(
            children[index]
                .iter()
                .rev()
                .map(|&child| (child, depth + 1)),
        );
    }
    ensure!(order.len() == rows.len(), "parent cycle in trace");

    let t0 = bounds.iter().map(|span| span.start).min().unwrap();
    let tmax = bounds.iter().map(Bounds::end_or_start).max().unwrap();
    let total = (tmax as i128 - t0 as i128).max(1);
    let mut output = String::new();
    writeln!(
        output,
        "{:>9} st  span{}│{:<width$}│",
        "dur",
        " ".repeat(22),
        "timeline →",
        width = WIDTH
    )?;
    writeln!(
        output,
        "{} ─  {}┼{}┤",
        "─".repeat(10),
        "─".repeat(LABEL_WIDTH),
        "─".repeat(WIDTH)
    )?;
    for (index, depth) in order {
        let span = &bounds[index];
        let duration = span.end_or_start() as i128 - span.start as i128;
        let offset =
            columns(span.start as i128 - t0 as i128, total).min((WIDTH - 1) as i128) as usize;
        let length = columns(duration, total)
            .max(1)
            .min((WIDTH - offset) as i128) as usize;
        let status = match rows[index].status {
            0 => '·',
            1 => '✓',
            2 => '✗',
            _ => '?',
        };
        writeln!(
            output,
            "{:8.1}ms {status}  {}│{}{}{}│",
            duration as f64 / 1_000_000.0,
            label(&rows[index].name, depth),
            " ".repeat(offset),
            "█".repeat(length),
            " ".repeat(WIDTH - offset - length)
        )?;
    }

    let negative = bounds
        .iter()
        .any(|span| span.raw_end.is_some_and(|end| end < span.raw_start));
    let mut violations = Vec::new();
    let mut violation_count = 0;
    for (index, parent) in parents.iter().enumerate() {
        let Some(parent) = parent else { continue };
        let child = &bounds[index];
        let parent_bounds = &bounds[*parent];
        let unclosed = parent_bounds.end_or_start() <= parent_bounds.start;
        if (child.start as i128) < parent_bounds.start as i128 - NESTING_SLACK
            || (!unclosed
                && child.end_or_start() as i128
                    > parent_bounds.end_or_start() as i128 + NESTING_SLACK)
        {
            violation_count += 1;
            if violations.len() < 5 {
                violations.push((index, *parent));
            }
        }
    }
    let verdict = |pass| if pass { "PASS ✓" } else { "FAIL ✗" };
    writeln!(output, "\nStatus: ✓ ok · unset ✗ error ? unknown")?;
    writeln!(output, "\nINTEGRITY")?;
    writeln!(output, "  spans total                 : {}", rows.len())?;
    writeln!(
        output,
        "  session-root present        : {} ({session_roots} root)",
        verdict(session_roots > 0)
    )?;
    writeln!(
        output,
        "  parent links resolved       : {} ({resolved}/{nonroot} non-root spans; {} dangling)",
        verdict(resolved == nonroot),
        nonroot - resolved
    )?;
    writeln!(
        output,
        "  no negative durations       : {}",
        verdict(!negative)
    )?;
    writeln!(
        output,
        "  children nest within parent : {} ({violation_count} violations)",
        verdict(violation_count == 0)
    )?;
    for (child, parent) in violations {
        writeln!(
            output,
            "      ! {} not within {}",
            label(&rows[child].name, 0).trim_end(),
            label(&rows[parent].name, 0).trim_end()
        )?;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::render;
    use crate::query_client::SpanRow;
    use unicode_width::UnicodeWidthStr;

    fn row(id: &str, name: &str, start: u64, end: Option<u64>, parent: Option<&str>) -> SpanRow {
        SpanRow {
            span_id: id.into(),
            trace_id: "test-trace".into(),
            parent_span_id: parent.map(str::to_owned),
            name: name.into(),
            kind: 1,
            start_unix_nano: start.to_string(),
            end_unix_nano: end.map(|v| v.to_string()),
            status: 1,
            session_id: None,
            harness: None,
            tool_name: None,
            tool_use_id: None,
            resource_json: None,
            attrs_json: None,
            effective_start_unix_nano: None,
            effective_end_unix_nano: None,
        }
    }

    fn span_lines(output: &str) -> Vec<&str> {
        output
            .lines()
            .skip(2)
            .take_while(|line| !line.is_empty())
            .collect()
    }

    fn bar(line: &str) -> &str {
        line.split('│').nth(1).unwrap()
    }

    #[test]
    fn renders_the_waterfall_layout_and_integrity_summary() {
        let session = row("s", "session", 0, Some(1_000_000_000), None);
        let mut turn = row("t", "turn:1", 50_000_000, Some(950_000_000), Some("s"));
        turn.status = 0;
        let mut tool = row("b", "Bash", 100_000_000, Some(300_000_000), Some("t"));
        tool.status = 2;
        let expected = format!(
            concat!(
                "      dur st  span                      │timeline →{}│\n",
                "{} ─  {}┼{}┤\n",
                "  1000.0ms ✓  session                   │{}│\n",
                "   900.0ms ·    turn:1                  │{}{}{}│\n",
                "   200.0ms ✗      Bash                  │{}{}{}│\n\n",
                "Status: ✓ ok · unset ✗ error ? unknown\n\n",
                "INTEGRITY\n",
                "  spans total                 : 3\n",
                "  session-root present        : PASS ✓ (1 root)\n",
                "  parent links resolved       : PASS ✓ (2/2 non-root spans; 0 dangling)\n",
                "  no negative durations       : PASS ✓\n",
                "  children nest within parent : PASS ✓ (0 violations)\n",
            ),
            " ".repeat(46),
            "─".repeat(10),
            "─".repeat(26),
            "─".repeat(56),
            "█".repeat(56),
            " ".repeat(3),
            "█".repeat(50),
            " ".repeat(3),
            " ".repeat(6),
            "█".repeat(11),
            " ".repeat(39),
        );
        assert_eq!(render(&[tool, session, turn]).unwrap(), expected);
    }

    #[test]
    fn effective_zero_bounds_are_used_instead_of_raw_bounds() {
        let mut session = row("s", "session", 500_000_000, Some(900_000_000), None);
        session.effective_start_unix_nano = Some("0".into());
        session.effective_end_unix_nano = Some("0".into());
        let child = row("b", "Bash", 50_000_000, Some(100_000_000), Some("s"));
        let output = render(&[session, child]).unwrap();
        let lines = span_lines(&output);
        assert!(lines[0].starts_with("     0.0ms"));
        assert_eq!(bar(lines[0]), format!("█{}", " ".repeat(55)));
        assert!(output.contains("children nest within parent : PASS ✓"));
    }

    #[test]
    fn raw_bounds_are_used_when_effective_bounds_are_missing_or_empty() {
        let mut session = row(
            "s",
            "session",
            1_800_000_000_000_000_000,
            Some(1_800_000_000_001_000_000),
            None,
        );
        session.effective_start_unix_nano = Some(String::new());
        session.effective_end_unix_nano = Some(String::new());
        let output = render(&[session]).unwrap();
        assert!(span_lines(&output)[0].starts_with("     1.0ms"));
        assert_eq!(bar(span_lines(&output)[0]), "█".repeat(56));
    }

    #[test]
    fn roots_and_siblings_are_ordered_by_raw_start_time() {
        let mut early = row("s", "session", 10, Some(100), None);
        early.effective_start_unix_nano = Some("1000".into());
        let late = row("l", "later-root", 20, Some(100), None);
        let mut first = row("a", "first-child", 30, Some(40), Some("s"));
        first.effective_start_unix_nano = Some("900".into());
        let second = row("b", "second-child", 40, Some(50), Some("s"));
        let output = render(&[late, second, first, early]).unwrap();
        let labels: Vec<_> = span_lines(&output)
            .iter()
            .map(|line| line.split('│').next().unwrap()[14..].trim())
            .collect();
        assert_eq!(
            labels,
            ["session", "first-child", "second-child", "later-root"]
        );
    }

    #[test]
    fn dangling_parents_render_without_losing_spans() {
        let child = row("b", "Bash", 1, Some(2), Some("missing"));
        let output = render(&[child]).unwrap();
        assert_eq!(span_lines(&output).len(), 1);
        assert!(output.contains("session-root present        : FAIL ✗ (0 root)"));
        assert!(output
            .contains("parent links resolved       : FAIL ✗ (0/1 non-root spans; 1 dangling)"));
    }

    #[test]
    fn negative_raw_duration_is_reported_even_with_valid_effective_bounds() {
        let mut session = row("s", "session", 100_000_000, Some(0), None);
        session.effective_start_unix_nano = Some("0".into());
        session.effective_end_unix_nano = Some("100000000".into());
        assert!(render(&[session])
            .unwrap()
            .contains("no negative durations       : FAIL ✗"));
    }

    #[test]
    fn nesting_accepts_exactly_two_milliseconds_of_slack() {
        let session = row("s", "session", 10_000_000, Some(100_000_000), None);
        let child = row("b", "Bash", 8_000_000, Some(102_000_000), Some("s"));
        assert!(render(&[session, child])
            .unwrap()
            .contains("children nest within parent : PASS ✓ (0 violations)"));
    }

    #[test]
    fn nesting_reports_bounds_beyond_the_slack() {
        let session = row("s", "session", 10_000_000, Some(100_000_000), None);
        let lower = row("a", "too-early", 7_999_999, Some(20_000_000), Some("s"));
        let upper = row("b", "too-late", 20_000_000, Some(102_000_001), Some("s"));
        let output = render(&[session, lower, upper]).unwrap();
        assert!(output.contains("children nest within parent : FAIL ✗ (2 violations)"));
        assert!(output.contains("! too-early not within session"));
        assert!(output.contains("! too-late not within session"));
    }

    #[test]
    fn unclosed_parents_only_enforce_the_lower_bound() {
        let session = row("s", "session", 10_000_000, None, None);
        let child = row("b", "Bash", 20_000_000, Some(500_000_000), Some("s"));
        assert!(render(&[session, child])
            .unwrap()
            .contains("children nest within parent : PASS ✓"));
    }

    #[test]
    fn empty_trace_is_an_error() {
        assert!(render(&[]).unwrap_err().to_string().contains("no spans"));
    }

    #[test]
    fn every_timestamp_field_is_validated() {
        for field in [
            "start_unix_nano",
            "end_unix_nano",
            "effective_start_unix_nano",
            "effective_end_unix_nano",
        ] {
            for bad in ["not-a-time", "-1", "18446744073709551616"] {
                let mut span = row("s", "session", 0, Some(1), None);
                match field {
                    "start_unix_nano" => span.start_unix_nano = bad.into(),
                    "end_unix_nano" => span.end_unix_nano = Some(bad.into()),
                    "effective_start_unix_nano" => {
                        span.effective_start_unix_nano = Some(bad.into())
                    }
                    _ => span.effective_end_unix_nano = Some(bad.into()),
                }
                let error = render(&[span]).unwrap_err().to_string();
                assert!(error.contains(field), "{field}: {error}");
            }
        }
    }

    #[test]
    fn duplicate_span_ids_are_an_error() {
        let span = row("s", "session", 0, Some(1), None);
        assert!(render(&[span.clone(), span])
            .unwrap_err()
            .to_string()
            .contains("duplicate span"));
    }

    #[test]
    fn cycles_are_rejected_even_when_a_separate_session_root_exists() {
        let session = row("s", "session", 0, Some(100), None);
        let a = row("a", "A", 1, Some(2), Some("b"));
        let b = row("b", "B", 2, Some(3), Some("a"));
        assert!(render(&[session, a, b])
            .unwrap_err()
            .to_string()
            .contains("cycle"));
        let self_parent = row("a", "A", 0, Some(1), Some("a"));
        assert!(render(&[self_parent])
            .unwrap_err()
            .to_string()
            .contains("cycle"));
    }

    #[test]
    fn deep_trees_are_iterative_and_labels_remain_bounded() {
        let mut rows = Vec::new();
        for i in 0..20_000 {
            let parent = (i > 0).then(|| (i - 1).to_string());
            rows.push(row(
                &i.to_string(),
                if i == 0 { "session" } else { "nested" },
                0,
                Some(1),
                parent.as_deref(),
            ));
        }
        let output = render(&rows).unwrap();
        let lines = span_lines(&output);
        assert_eq!(lines.len(), rows.len());
        assert_eq!(
            lines
                .last()
                .unwrap()
                .split('│')
                .next()
                .unwrap()
                .chars()
                .count(),
            40
        );
        assert_eq!(bar(lines.last().unwrap()).chars().count(), 56);
        assert!(output.contains("spans total                 : 20000"));
    }

    #[test]
    fn labels_and_integrity_diagnostics_cannot_inject_terminal_controls() {
        let session = row("s", "session", 10_000_000, Some(100_000_000), None);
        let mut child = row(
            "b",
            "Bash\u{1b}[31m\n\r\t\u{7f}\u{9b}\u{202e}\u{2066}injected",
            0,
            Some(1),
            Some("s"),
        );
        child.status = 99;
        let output = render(&[session, child]).unwrap();
        assert!(span_lines(&output)[1].contains("ms ?"));
        assert_eq!(span_lines(&output).len(), 2);
        assert!(!output.chars().any(|c| c.is_control() && c != '\n'));
        assert!(!output.contains(['\u{202e}', '\u{2066}']));
        assert_eq!(
            output
                .lines()
                .filter(|line| line.contains("not within"))
                .count(),
            1
        );
    }

    #[test]
    fn timing_bars_use_python_ties_to_even_rounding() {
        let session = row("s", "session", 0, Some(112), None);
        let low_tie = row("a", "low", 1, Some(2), Some("s"));
        let high_tie = row("b", "high", 3, Some(4), Some("s"));
        let output = render(&[session, low_tie, high_tie]).unwrap();
        let lines = span_lines(&output);
        assert!(bar(lines[1]).starts_with('█'));
        assert!(bar(lines[2]).starts_with("  █"));
    }

    #[test]
    fn unicode_labels_keep_the_timeline_at_column_forty() {
        for name in [
            "界".repeat(30),
            "e\u{301}".repeat(30),
            "👩\u{200d}💻".repeat(30),
            "🇬🇧".repeat(30),
            "界e\u{301}👩\u{200d}💻 mixed label".into(),
        ] {
            let mut rows = vec![row("s", "session", 0, Some(100), None)];
            for depth in 1..=20 {
                let id = depth.to_string();
                let parent = if depth == 1 {
                    "s".into()
                } else {
                    (depth - 1).to_string()
                };
                rows.push(row(&id, &name, 1, Some(2), Some(&parent)));
            }
            let output = render(&rows).unwrap();
            for line in span_lines(&output) {
                let prefix = line.split('│').next().unwrap();
                assert_eq!(UnicodeWidthStr::width(prefix), 40, "{prefix:?}");
                assert_eq!(UnicodeWidthStr::width(bar(line)), 56);
            }
        }
    }

    #[test]
    fn header_separator_and_spans_align_both_timeline_borders() {
        let session = row("s", "session", 0, Some(1_000_000_000), None);
        let child = row(
            "b",
            "界e\u{301}👩\u{200d}💻",
            100_000_000,
            Some(300_000_000),
            Some("s"),
        );
        let output = render(&[session, child]).unwrap();
        let border_columns = |line: &str| {
            line.char_indices()
                .filter(|(_, ch)| matches!(ch, '│' | '┼' | '┤'))
                .map(|(index, _)| UnicodeWidthStr::width(&line[..index]))
                .collect::<Vec<_>>()
        };
        let expected = border_columns(output.lines().next().unwrap());
        assert_eq!(expected.len(), 2);
        for line in output.lines().take_while(|line| !line.is_empty()) {
            assert_eq!(border_columns(line), expected, "misaligned line: {line}");
        }
    }

    #[test]
    fn unicode_graphemes_are_preserved_at_the_label_boundary() {
        let combining = format!("{}e\u{301}", "x".repeat(25));
        let emoji = format!("{}👩\u{200d}💻", "x".repeat(25));
        for (name, expected) in [
            (combining.clone(), combining),
            (emoji, format!("{}…", "x".repeat(25))),
        ] {
            let output = render(&[row("s", &name, 0, Some(1), None)]).unwrap();
            let label: String = span_lines(&output)[0]
                .split('│')
                .next()
                .unwrap()
                .chars()
                .skip(14)
                .collect();
            assert_eq!(label, expected);
        }
        let mut rows = vec![row("s", "session", 0, Some(1), None)];
        for depth in 1..=20 {
            let id = depth.to_string();
            let parent = if depth == 1 {
                "s".into()
            } else {
                (depth - 1).to_string()
            };
            rows.push(row(&id, "👩\u{200d}💻", 0, Some(1), Some(&parent)));
        }
        let output = render(&rows).unwrap();
        let label: String = span_lines(&output)
            .last()
            .unwrap()
            .split('│')
            .next()
            .unwrap()
            .chars()
            .skip(14)
            .collect();
        assert_eq!(label, format!("{}👩\u{200d}💻", " ".repeat(24)));
    }

    #[test]
    fn unicode_oversized_graphemes_produce_bounded_aligned_output() {
        let name = format!("e{}", "\u{301}".repeat(100_000));
        let output = render(&[row("s", &name, 0, Some(1), None)]).unwrap();
        let prefix = span_lines(&output)[0].split('│').next().unwrap();
        assert_eq!(UnicodeWidthStr::width(prefix), 40);
        assert!(output.len() < 1500);
    }

    #[test]
    fn clipped_labels_end_in_ellipsis_without_splitting_graphemes() {
        for (name, expected) in [
            ("x".repeat(30), format!("{}…", "x".repeat(25))),
            ("界".repeat(30), format!("{}… ", "界".repeat(12))),
            ("e\u{301}".repeat(30), format!("{}…", "e\u{301}".repeat(25))),
        ] {
            let output = render(&[row("s", &name, 0, Some(1), None)]).unwrap();
            let label: String = span_lines(&output)[0]
                .split('│')
                .next()
                .unwrap()
                .chars()
                .skip(14)
                .collect();
            assert_eq!(label, expected);
            assert_eq!(UnicodeWidthStr::width(label.as_str()), 26);
        }
    }

    #[test]
    fn deep_clipped_labels_keep_indentation_and_ellipsis() {
        for (name, expected) in [
            ("abcdef", "a…"),
            ("界界", "… "),
            ("e\u{301}e\u{301}e\u{301}", "e\u{301}…"),
        ] {
            let mut rows = vec![row("s", "session", 0, Some(1), None)];
            for depth in 1..=20 {
                let id = depth.to_string();
                let parent = if depth == 1 {
                    "s".into()
                } else {
                    (depth - 1).to_string()
                };
                rows.push(row(&id, name, 0, Some(1), Some(&parent)));
            }
            let output = render(&rows).unwrap();
            let label: String = span_lines(&output)
                .last()
                .unwrap()
                .split('│')
                .next()
                .unwrap()
                .chars()
                .skip(14)
                .collect();
            assert_eq!(label, format!("{}{expected}", " ".repeat(24)));
            assert_eq!(UnicodeWidthStr::width(label.as_str()), 26);
        }
    }

    #[test]
    fn exact_fit_labels_have_no_ellipsis() {
        for name in [
            "x".repeat(26),
            "界".repeat(13),
            "e\u{301}".repeat(26),
            "👩\u{200d}💻".repeat(13),
        ] {
            let output = render(&[row("s", &name, 0, Some(1), None)]).unwrap();
            let label: String = span_lines(&output)[0]
                .split('│')
                .next()
                .unwrap()
                .chars()
                .skip(14)
                .collect();
            assert_eq!(label, name);
        }
    }

    #[test]
    fn status_legend_identifies_unset_and_unknown_states() {
        let mut rows = Vec::new();
        for status in [0, 1, 2, 99] {
            let mut span = row(&status.to_string(), "status", 0, Some(1), None);
            span.status = status;
            rows.push(span);
        }
        let output = render(&rows).unwrap();
        let lines = span_lines(&output);
        for (line, symbol) in lines.iter().zip(['·', '✓', '✗', '?']) {
            assert!(line.contains(&format!("ms {symbol}  ")));
        }
        assert_eq!(
            output
                .matches("Status: ✓ ok · unset ✗ error ? unknown")
                .count(),
            1
        );
        assert!(output.contains("\n\nStatus: ✓ ok · unset ✗ error ? unknown\n\nINTEGRITY\n"));
    }
}
