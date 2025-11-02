use std::time::Instant;

use super::model::CommandOutput;
use super::model::ExecCall;
use super::model::ExecCell;
use crate::exec_command::relativize_to_home;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::history_cell::HistoryCell;
use crate::render::highlight::highlight_bash_to_lines;
use crate::render::line_utils::prefix_lines;
use crate::render::line_utils::push_owned_lines;
use crate::shimmer::shimmer_spans;
use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_line;
use crate::wrapping::word_wrap_lines;
use codex_ansi_escape::ansi_escape_line;
use codex_common::elapsed::format_duration;
use codex_protocol::parse_command::ParsedCommand;
use itertools::Itertools;
use ratatui::prelude::*;
use ratatui::style::Modifier;
use ratatui::style::Stylize;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;
use ratatui::widgets::Wrap;
use textwrap::WordSplitter;
use unicode_width::UnicodeWidthStr;

pub(crate) const TOOL_CALL_MAX_LINES: usize = 5;

pub(crate) struct OutputLinesParams {
    pub(crate) include_angle_pipe: bool,
    pub(crate) include_prefix: bool,
}

pub(crate) fn new_active_exec_command(
    call_id: String,
    command: Vec<String>,
    parsed: Vec<ParsedCommand>,
) -> ExecCell {
    ExecCell::new(ExecCall {
        call_id,
        command,
        parsed,
        output: None,
        start_time: Some(Instant::now()),
        duration: None,
    })
}

#[derive(Clone)]
pub(crate) struct OutputLines {
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) omitted: Option<usize>,
}

pub(crate) fn output_lines(
    output: Option<&CommandOutput>,
    params: OutputLinesParams,
) -> OutputLines {
    let OutputLinesParams {
        include_angle_pipe,
        include_prefix,
    } = params;
    let CommandOutput {
        aggregated_output, ..
    } = match output {
        Some(output) => output,
        None => {
            return OutputLines {
                lines: Vec::new(),
                omitted: None,
            };
        }
    };

    let src = aggregated_output;
    let lines: Vec<&str> = src.lines().collect();
    let total = lines.len();
    let limit = TOOL_CALL_MAX_LINES;

    let mut out: Vec<Line<'static>> = Vec::new();

    let head_end = total.min(limit);
    for (i, raw) in lines[..head_end].iter().enumerate() {
        let mut line = ansi_escape_line(raw);
        let prefix = if !include_prefix {
            ""
        } else if i == 0 && include_angle_pipe {
            "  └ "
        } else {
            "    "
        };
        line.spans.insert(0, prefix.into());
        line.spans.iter_mut().for_each(|span| {
            span.style = span.style.add_modifier(Modifier::DIM);
        });
        out.push(line);
    }

    let show_ellipsis = total > 2 * limit;
    let omitted = if show_ellipsis {
        Some(total - 2 * limit)
    } else {
        None
    };
    if show_ellipsis {
        let omitted = total - 2 * limit;
        out.push(format!("… +{omitted} lines").into());
    }

    let tail_start = if show_ellipsis {
        total - limit
    } else {
        head_end
    };
    for raw in lines[tail_start..].iter() {
        let mut line = ansi_escape_line(raw);
        if include_prefix {
            line.spans.insert(0, "    ".into());
        }
        line.spans.iter_mut().for_each(|span| {
            span.style = span.style.add_modifier(Modifier::DIM);
        });
        out.push(line);
    }

    OutputLines {
        lines: out,
        omitted,
    }
}

pub(crate) fn spinner(start_time: Option<Instant>) -> Span<'static> {
    let elapsed = start_time.map(|st| st.elapsed()).unwrap_or_default();
    if supports_color::on_cached(supports_color::Stream::Stdout)
        .map(|level| level.has_16m)
        .unwrap_or(false)
    {
        shimmer_spans("•")[0].clone()
    } else {
        let blink_on = (elapsed.as_millis() / 600).is_multiple_of(2);
        if blink_on { "•".into() } else { "◦".dim() }
    }
}

impl HistoryCell for ExecCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        if self.is_exploring_cell() {
            self.exploring_display_lines(width)
        } else {
            self.command_display_lines(width)
        }
    }

    fn desired_transcript_height(&self, width: u16) -> u16 {
        self.transcript_lines(width).len() as u16
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = vec![];
        for (i, call) in self.iter_calls().enumerate() {
            if i > 0 {
                lines.push("".into());
            }
            let script = strip_bash_lc_and_escape(&call.command);
            let highlighted_script = highlight_bash_to_lines(&script);
            let cmd_display = word_wrap_lines(
                &highlighted_script,
                RtOptions::new(width as usize)
                    .initial_indent("$ ".magenta().into())
                    .subsequent_indent("    ".into()),
            );
            lines.extend(cmd_display);

            if let Some(output) = call.output.as_ref() {
                lines.extend(output.formatted_output.lines().map(ansi_escape_line));
                let duration = call
                    .duration
                    .map(format_duration)
                    .unwrap_or_else(|| "unknown".to_string());
                let mut result: Line = if output.exit_code == 0 {
                    Line::from("✓".green().bold())
                } else {
                    Line::from(vec![
                        "✗".red().bold(),
                        format!(" ({})", output.exit_code).into(),
                    ])
                };
                result.push_span(format!(" • {duration}").dim());
                lines.push(result);
            }
        }
        lines
    }
}

impl WidgetRef for &ExecCell {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        let content_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: area.height,
        };
        let lines = self.display_lines(area.width);
        let max_rows = area.height as usize;
        let rendered = if lines.len() > max_rows {
            lines[lines.len() - max_rows..].to_vec()
        } else {
            lines
        };

        Paragraph::new(Text::from(rendered))
            .wrap(Wrap { trim: false })
            .render(content_area, buf);
    }
}

impl ExecCell {
    fn exploring_display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();
        out.push(Line::from(vec![
            if self.is_active() {
                spinner(self.active_start_time())
            } else {
                "•".dim()
            },
            " ".into(),
            if self.is_active() {
                "Exploring".bold()
            } else {
                "Explored".bold()
            },
        ]));

        let mut calls = self.calls.clone();
        let mut out_indented = Vec::new();
        while !calls.is_empty() {
            let mut call = calls.remove(0);
            if call
                .parsed
                .iter()
                .all(|parsed| matches!(parsed, ParsedCommand::Read { .. }))
            {
                while let Some(next) = calls.first() {
                    if next
                        .parsed
                        .iter()
                        .all(|parsed| matches!(parsed, ParsedCommand::Read { .. }))
                    {
                        call.parsed.extend(next.parsed.clone());
                        calls.remove(0);
                    } else {
                        break;
                    }
                }
            }

            let reads_only = call
                .parsed
                .iter()
                .all(|parsed| matches!(parsed, ParsedCommand::Read { .. }));

            let call_lines: Vec<(&str, Vec<Span<'static>>)> = if reads_only {
                let names = call
                    .parsed
                    .iter()
                    .map(|parsed| match parsed {
                        ParsedCommand::Read { name, .. } => name.clone(),
                        _ => unreachable!(),
                    })
                    .unique();
                vec![(
                    "Read",
                    Itertools::intersperse(names.into_iter().map(Into::into), ", ".dim()).collect(),
                )]
            } else {
                let mut lines = Vec::new();
                for parsed in &call.parsed {
                    match parsed {
                        ParsedCommand::Read { name, .. } => {
                            lines.push(("Read", vec![name.clone().into()]));
                        }
                        ParsedCommand::ListFiles { cmd, path } => {
                            lines.push(("List", vec![path.clone().unwrap_or(cmd.clone()).into()]));
                        }
                        ParsedCommand::Search { cmd, query, path } => {
                            let spans = match (query, path) {
                                (Some(q), Some(p)) => {
                                    vec![q.clone().into(), " in ".dim(), p.clone().into()]
                                }
                                (Some(q), None) => vec![q.clone().into()],
                                _ => vec![cmd.clone().into()],
                            };
                            lines.push(("Search", spans));
                        }
                        ParsedCommand::Unknown { cmd } => {
                            if let Some((label, spans)) = summarize_unknown_command(cmd) {
                                lines.push((label, spans));
                            } else {
                                lines.push(("Run", vec![cmd.clone().into()]));
                            }
                        }
                    }
                }
                lines
            };

            for (title, line) in call_lines {
                let line = Line::from(line);
                let initial_indent = Line::from(vec![title.cyan(), " ".into()]);
                let subsequent_indent = " ".repeat(initial_indent.width()).into();
                let wrapped = word_wrap_line(
                    &line,
                    RtOptions::new(width as usize)
                        .initial_indent(initial_indent)
                        .subsequent_indent(subsequent_indent),
                );
                push_owned_lines(&wrapped, &mut out_indented);
            }
        }

        out.extend(prefix_lines(out_indented, "  └ ".dim(), "    ".into()));
        out
    }

    fn command_display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let [call] = &self.calls.as_slice() else {
            panic!("Expected exactly one call in a command display cell");
        };
        let layout = EXEC_DISPLAY_LAYOUT;
        let success = call.output.as_ref().map(|o| o.exit_code == 0);
        let bullet = match success {
            Some(true) => "•".green().bold(),
            Some(false) => "•".red().bold(),
            None => spinner(call.start_time),
        };
        let title = if self.is_active() { "Running" } else { "Ran" };

        let mut header_line =
            Line::from(vec![bullet.clone(), " ".into(), title.bold(), " ".into()]);
        let header_prefix_width = header_line.width();

        let raw_script = strip_bash_lc_and_escape(&call.command);
        let heredoc = detect_cat_heredoc(&raw_script);
        let script_for_display = heredoc
            .as_ref()
            .map(|info| info.command_line.as_str())
            .unwrap_or(&raw_script);
        let highlighted_lines = highlight_bash_to_lines(script_for_display);

        let continuation_wrap_width = layout.command_continuation.wrap_width(width);
        let continuation_opts =
            RtOptions::new(continuation_wrap_width).word_splitter(WordSplitter::NoHyphenation);

        let mut continuation_lines: Vec<Line<'static>> = Vec::new();

        if let Some((first, rest)) = highlighted_lines.split_first() {
            let available_first_width = (width as usize).saturating_sub(header_prefix_width).max(1);
            let first_opts =
                RtOptions::new(available_first_width).word_splitter(WordSplitter::NoHyphenation);
            let mut first_wrapped: Vec<Line<'static>> = Vec::new();
            push_owned_lines(&word_wrap_line(first, first_opts), &mut first_wrapped);
            let mut first_wrapped_iter = first_wrapped.into_iter();
            if let Some(first_segment) = first_wrapped_iter.next() {
                header_line.extend(first_segment);
            }
            continuation_lines.extend(first_wrapped_iter);

            for line in rest {
                push_owned_lines(
                    &word_wrap_line(line, continuation_opts.clone()),
                    &mut continuation_lines,
                );
            }
        }

        let mut lines: Vec<Line<'static>> = vec![header_line];

        let continuation_lines = Self::limit_lines_from_start(
            &continuation_lines,
            layout.command_continuation_max_lines,
        );
        if !continuation_lines.is_empty() {
            lines.extend(prefix_lines(
                continuation_lines,
                Span::from(layout.command_continuation.initial_prefix).dim(),
                Span::from(layout.command_continuation.subsequent_prefix).dim(),
            ));
        }

        if let Some(info) = heredoc.as_ref() {
            let mut summary_spans: Vec<Span<'static>> = Vec::new();
            let label = if info.append { "Append" } else { "Write" };
            summary_spans.push(label.cyan());
            summary_spans.push(" ".into());
            summary_spans.push(format_path_span(&info.target));
            if let Some(count) = pluralize_lines(info.line_count) {
                summary_spans.push(" ".into());
                summary_spans.push(Span::from(format!("({count})")).dim());
            }
            let summary_line = Line::from(summary_spans);
            lines.extend(prefix_lines(
                vec![summary_line],
                Span::from(layout.output_block.initial_prefix).dim(),
                Span::from(layout.output_block.subsequent_prefix),
            ));
        }

        if let Some(output) = call.output.as_ref() {
            let raw_output = output_lines(
                Some(output),
                OutputLinesParams {
                    include_angle_pipe: false,
                    include_prefix: false,
                },
            );

            if raw_output.lines.is_empty() {
                lines.extend(prefix_lines(
                    vec![Line::from("(no output)".dim())],
                    Span::from(layout.output_block.initial_prefix).dim(),
                    Span::from(layout.output_block.subsequent_prefix),
                ));
            } else {
                let trimmed_output = Self::truncate_lines_middle(
                    &raw_output.lines,
                    layout.output_max_lines,
                    raw_output.omitted,
                );

                let mut wrapped_output: Vec<Line<'static>> = Vec::new();
                let output_wrap_width = layout.output_block.wrap_width(width);
                let output_opts =
                    RtOptions::new(output_wrap_width).word_splitter(WordSplitter::NoHyphenation);
                for line in trimmed_output {
                    push_owned_lines(
                        &word_wrap_line(&line, output_opts.clone()),
                        &mut wrapped_output,
                    );
                }

                if !wrapped_output.is_empty() {
                    lines.extend(prefix_lines(
                        wrapped_output,
                        Span::from(layout.output_block.initial_prefix).dim(),
                        Span::from(layout.output_block.subsequent_prefix),
                    ));
                }
            }
        }

        lines
    }

    fn limit_lines_from_start(lines: &[Line<'static>], keep: usize) -> Vec<Line<'static>> {
        if lines.len() <= keep {
            return lines.to_vec();
        }
        if keep == 0 {
            return vec![Self::ellipsis_line(lines.len())];
        }

        let mut out: Vec<Line<'static>> = lines[..keep].to_vec();
        out.push(Self::ellipsis_line(lines.len() - keep));
        out
    }

    fn truncate_lines_middle(
        lines: &[Line<'static>],
        max: usize,
        omitted_hint: Option<usize>,
    ) -> Vec<Line<'static>> {
        if max == 0 {
            return Vec::new();
        }
        if lines.len() <= max {
            return lines.to_vec();
        }
        if max == 1 {
            // Carry forward any previously omitted count and add any
            // additionally hidden content lines from this truncation.
            let base = omitted_hint.unwrap_or(0);
            // When an existing ellipsis is present, `lines` already includes
            // that single representation line; exclude it from the count of
            // additionally omitted content lines.
            let extra = lines
                .len()
                .saturating_sub(usize::from(omitted_hint.is_some()));
            let omitted = base + extra;
            return vec![Self::ellipsis_line(omitted)];
        }

        let head = (max - 1) / 2;
        let tail = max - head - 1;
        let mut out: Vec<Line<'static>> = Vec::new();

        if head > 0 {
            out.extend(lines[..head].iter().cloned());
        }

        let base = omitted_hint.unwrap_or(0);
        let additional = lines
            .len()
            .saturating_sub(head + tail)
            .saturating_sub(usize::from(omitted_hint.is_some()));
        out.push(Self::ellipsis_line(base + additional));

        if tail > 0 {
            out.extend(lines[lines.len() - tail..].iter().cloned());
        }

        out
    }

    fn ellipsis_line(omitted: usize) -> Line<'static> {
        Line::from(vec![format!("… +{omitted} lines").dim()])
    }
}

#[derive(Debug)]
struct HeredocInfo {
    command_line: String,
    target: String,
    line_count: usize,
    append: bool,
}

fn summarize_unknown_command(cmd: &str) -> Option<(&'static str, Vec<Span<'static>>)> {
    let info = detect_cat_heredoc(cmd)?;
    let label = if info.append { "Append" } else { "Write" };
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(format_path_span(&info.target));
    if let Some(count) = pluralize_lines(info.line_count) {
        spans.push(" ".into());
        spans.push(Span::from(format!("({count})")).dim());
    }
    Some((label, spans))
}

fn detect_cat_heredoc(script: &str) -> Option<HeredocInfo> {
    let mut lines = script.lines();
    let first_line = lines.next()?.trim();
    let trimmed_start = first_line.trim_start();
    if !trimmed_start.starts_with("cat ") || !trimmed_start.contains("<<") {
        return None;
    }
    let target = extract_redirection_target(first_line)?;
    let terminator = parse_heredoc_terminator(first_line)?;
    let append = trimmed_start.contains(">>");

    let mut line_count = 0usize;
    let mut found_end = false;
    for line in lines {
        if line.trim() == terminator {
            found_end = true;
            break;
        }
        line_count += 1;
    }
    if !found_end {
        return None;
    }

    Some(HeredocInfo {
        command_line: first_line.to_string(),
        target,
        line_count,
        append,
    })
}

fn parse_heredoc_terminator(line: &str) -> Option<String> {
    let idx = line.find("<<")?;
    let mut rest = &line[idx + 2..];
    rest = rest.trim_start();
    if let Some(stripped) = rest.strip_prefix('-') {
        rest = stripped.trim_start();
    }
    let mut chars = rest.chars();
    let first = chars.next()?;
    if first == '\'' || first == '"' {
        let quote = first;
        let mut terminator = String::new();
        for ch in chars {
            if ch == quote {
                break;
            }
            terminator.push(ch);
        }
        if terminator.is_empty() {
            None
        } else {
            Some(terminator)
        }
    } else {
        let mut terminator = first.to_string();
        for ch in chars {
            if ch.is_whitespace() || ch == '>' {
                break;
            }
            terminator.push(ch);
        }
        Some(terminator)
    }
}

fn extract_redirection_target(line: &str) -> Option<String> {
    let idx = line.rfind('>')?;
    if idx + 1 >= line.len() {
        return None;
    }
    let mut segment = line[idx + 1..].trim();
    if segment.is_empty() || segment.starts_with('&') {
        return None;
    }
    while segment.starts_with('>') {
        segment = segment[1..].trim_start();
    }
    if segment.starts_with('&') || segment.is_empty() {
        return None;
    }
    if segment.starts_with('"') || segment.starts_with('\'') {
        let mut chars = segment.chars();
        let quote = chars.next()?;
        let mut collected = String::new();
        for ch in chars {
            if ch == quote {
                break;
            }
            collected.push(ch);
        }
        if collected.is_empty() {
            None
        } else {
            Some(collected)
        }
    } else {
        let token = segment
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        if token.is_empty() { None } else { Some(token) }
    }
}

fn format_path_span(path: &str) -> Span<'static> {
    let display = format_path_for_display(path);
    let adjusted = if display.contains(char::is_whitespace) {
        format!("\"{display}\"")
    } else {
        display
    };
    adjusted.into()
}

fn format_path_for_display(path: &str) -> String {
    let trimmed = path.trim().trim_matches('"').trim_matches('\'');
    if let Some(rel) = relativize_to_home(trimmed) {
        let rel_str = rel.to_string_lossy();
        if rel_str.is_empty() {
            "~".to_string()
        } else {
            format!("~{rel_str}")
        }
    } else {
        trimmed.to_string()
    }
}

fn pluralize_lines(count: usize) -> Option<String> {
    if count == 0 {
        None
    } else if count == 1 {
        Some("1 line".to_string())
    } else {
        Some(format!("{count} lines"))
    }
}

#[derive(Clone, Copy)]
struct PrefixedBlock {
    initial_prefix: &'static str,
    subsequent_prefix: &'static str,
}

impl PrefixedBlock {
    const fn new(initial_prefix: &'static str, subsequent_prefix: &'static str) -> Self {
        Self {
            initial_prefix,
            subsequent_prefix,
        }
    }

    fn wrap_width(self, total_width: u16) -> usize {
        let prefix_width = UnicodeWidthStr::width(self.initial_prefix)
            .max(UnicodeWidthStr::width(self.subsequent_prefix));
        usize::from(total_width).saturating_sub(prefix_width).max(1)
    }
}

#[derive(Clone, Copy)]
struct ExecDisplayLayout {
    command_continuation: PrefixedBlock,
    command_continuation_max_lines: usize,
    output_block: PrefixedBlock,
    output_max_lines: usize,
}

impl ExecDisplayLayout {
    const fn new(
        command_continuation: PrefixedBlock,
        command_continuation_max_lines: usize,
        output_block: PrefixedBlock,
        output_max_lines: usize,
    ) -> Self {
        Self {
            command_continuation,
            command_continuation_max_lines,
            output_block,
            output_max_lines,
        }
    }
}

const EXEC_DISPLAY_LAYOUT: ExecDisplayLayout = ExecDisplayLayout::new(
    PrefixedBlock::new("  │ ", "  │ "),
    2,
    PrefixedBlock::new("  └ ", "    "),
    5,
);
