use std::time::Instant;

use super::model::CommandOutput;
use super::model::ExecCall;
use super::model::ExecCell;
use super::model::ExecStreamKind;
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
use codex_core::heredoc::{self, HeredocAction, HeredocMetadata};
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
const STREAM_SECTION_INDENT: &str = "    ";
const STREAM_PREVIEW_LINES: usize = 3;
const STREAM_EXPANDED_LINE_CAP: usize = 400;

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
    let heredoc = heredoc::analyze(&script);
            let script_for_display = heredoc
                .as_ref()
                .map(|info| info.command_line.as_str())
                .unwrap_or(&script);
            let highlighted_script = highlight_bash_to_lines(script_for_display);
            let cmd_display = word_wrap_lines(
                &highlighted_script,
                RtOptions::new(width as usize)
                    .initial_indent("$ ".magenta().into())
                    .subsequent_indent("    ".into()),
            );
            lines.extend(cmd_display);

            if let Some(info) = heredoc.as_ref() {
                let summary_line = heredoc_summary_line_from_metadata(info);
                let prefix_block = PrefixedBlock::new("  └ ", "    ");
                let wrap_width = prefix_block.wrap_width(width);
                let mut summary_wrapped: Vec<Line<'static>> = Vec::new();
                push_owned_lines(
                    &word_wrap_line(
                        &summary_line,
                        RtOptions::new(wrap_width).word_splitter(WordSplitter::NoHyphenation),
                    ),
                    &mut summary_wrapped,
                );
                lines.extend(prefix_lines(
                    summary_wrapped,
                    Span::from(prefix_block.initial_prefix).dim(),
                    Span::from(prefix_block.subsequent_prefix),
                ));
            }

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

                lines.extend(render_stream_sections(output, width));
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
                        ParsedCommand::Write {
                            targets,
                            append,
                            line_count,
                            ..
                        } => {
                            let mut spans: Vec<Span<'static>> = Vec::new();
                            if let Some(first) = targets.first() {
                                spans.push(format_path_span(first));
                                if targets.len() > 1 {
                                    spans.push(" ".into());
                                    spans.push(
                                        Span::from(format!("(+{} more)", targets.len() - 1)).dim()
                                    );
                                }
                            }
                            if let Some(count) = line_count.and_then(|c| pluralize_lines(c)) {
                                spans.push(" ".into());
                                spans.push(Span::from(format!("({count})")).dim());
                            }
                            let label = if *append { "Append" } else { "Write" };
                            lines.push((label, spans));
                        }
                        ParsedCommand::Run {
                            program,
                            line_count,
                            ..
                        } => {
                            let mut spans: Vec<Span<'static>> = vec![program.clone().into()];
                            if let Some(count) = line_count.and_then(|c| pluralize_lines(c)) {
                                spans.push(" ".into());
                                spans.push(Span::from(format!("({count})")).dim());
                            }
                            lines.push(("Run", spans));
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
        let heredoc = heredoc::analyze(&raw_script);
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
            let summary_line = heredoc_summary_line_from_metadata(info);
            let mut wrapped_block: Vec<Line<'static>> = Vec::new();
            let wrap_width = layout.output_block.wrap_width(width);
            let wrap_opts = RtOptions::new(wrap_width).word_splitter(WordSplitter::NoHyphenation);
            push_owned_lines(
                &word_wrap_line(&summary_line, wrap_opts),
                &mut wrapped_block,
            );
            lines.extend(prefix_lines(
                wrapped_block,
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
struct HeredocSummary {
    label: HeredocLabel,
    detail_spans: Vec<Span<'static>>,
}

#[derive(Debug, Clone, Copy)]
enum HeredocLabel {
    Write,
    Append,
    Run,
}

impl HeredocLabel {
    fn as_str(self) -> &'static str {
        match self {
            HeredocLabel::Write => "Write",
            HeredocLabel::Append => "Append",
            HeredocLabel::Run => "Run",
        }
    }
}

fn heredoc_summary_from_metadata(meta: &HeredocMetadata) -> HeredocSummary {
    match meta.action {
        HeredocAction::Run => {
            let mut detail_spans: Vec<Span<'static>> = Vec::new();
            detail_spans.push(meta.command.clone().into());
            if let Some(count) = pluralize_lines(meta.line_count) {
                detail_spans.push(" ".into());
                detail_spans.push(Span::from(format!("({count})")).dim());
            }
            HeredocSummary {
                label: HeredocLabel::Run,
                detail_spans,
            }
        }
        HeredocAction::Write { append } => {
            let mut detail_spans = heredoc_target_spans(meta);
            if let Some(count) = pluralize_lines(meta.line_count) {
                detail_spans.push(" ".into());
                detail_spans.push(Span::from(format!("({count})")).dim());
            }
            let label = if append {
                HeredocLabel::Append
            } else {
                HeredocLabel::Write
            };
            HeredocSummary {
                label,
                detail_spans,
            }
        }
    }
}

fn heredoc_summary_line_from_metadata(meta: &HeredocMetadata) -> Line<'static> {
    let summary = heredoc_summary_from_metadata(meta);
    let mut spans: Vec<Span<'static>> = vec![summary.label.as_str().cyan()];
    if !summary.detail_spans.is_empty() {
        spans.push(" ".into());
        spans.extend(summary.detail_spans);
    }
    Line::from(spans)
}

fn heredoc_target_spans(meta: &HeredocMetadata) -> Vec<Span<'static>> {
    let mut detail_spans: Vec<Span<'static>> = Vec::new();
    if let Some(first) = meta.targets.first() {
        detail_spans.push(format_path_span(&first.path));
        if meta.targets.len() > 1 {
            detail_spans.push(" ".into());
            detail_spans.push(Span::from(format!("(+{} more)", meta.targets.len() - 1)).dim());
        }
    }
    detail_spans
}

fn render_stream_sections(output: &CommandOutput, width: u16) -> Vec<Line<'static>> {
    let mut sections: Vec<Line<'static>> = Vec::new();
    let mut wrote_stdout = false;
    if !output.stdout.is_empty() {
        sections.extend(stream_section_lines(
            width,
            ExecStreamKind::Stdout,
            &output.stdout,
            output.stdout_collapsed,
        ));
        wrote_stdout = !sections.is_empty();
    }
    if !output.stderr.is_empty() {
        if wrote_stdout {
            sections.push(Line::from(""));
        }
        sections.extend(stream_section_lines(
            width,
            ExecStreamKind::Stderr,
            &output.stderr,
            output.stderr_collapsed,
        ));
    }
    sections
}

fn stream_section_lines(
    width: u16,
    kind: ExecStreamKind,
    content: &str,
    collapsed: bool,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let indicator = if collapsed { "▶".dim() } else { "▼".dim() };
    let label_span = match kind {
        ExecStreamKind::Stdout => "stdout".green().bold(),
        ExecStreamKind::Stderr => "stderr".red().bold(),
    };
    let line_count = content.lines().count();
    let summary = format!("({line_count} {})", pluralize_line_word(line_count)).dim();
    lines.push(Line::from(vec![
        indicator,
        " ".into(),
        label_span,
        " ".into(),
        summary,
    ]));

    if content.trim().is_empty() {
        lines.push(format!("{STREAM_SECTION_INDENT}<empty>").dim().into());
        lines.push(stream_hint_line(kind, collapsed));
        return lines;
    }

    if collapsed {
        let mut preview_iter = content.lines();
        for _ in 0..STREAM_PREVIEW_LINES {
            if let Some(preview_line) = preview_iter.next() {
                lines.push(
                    format!("{STREAM_SECTION_INDENT}{preview_line}")
                        .dim()
                        .into(),
                );
            } else {
                break;
            }
        }
        let remaining = line_count.saturating_sub(STREAM_PREVIEW_LINES);
        if remaining > 0 {
            lines.push(
                format!(
                    "{STREAM_SECTION_INDENT}… +{remaining} more {}",
                    pluralize_line_word(remaining)
                )
                .dim()
                .into(),
            );
        }
        lines.push(stream_hint_line(kind, collapsed));
        return lines;
    }

    let indent_span: Span<'static> = STREAM_SECTION_INDENT.to_string().into();
    let available_width = width
        .saturating_sub(STREAM_SECTION_INDENT.len() as u16)
        .max(1);
    let mut wrapped = word_wrap_lines(
        &content
            .lines()
            .map(|line| colorize_stream_line(kind, line))
            .collect::<Vec<_>>(),
        RtOptions::new(available_width as usize),
    );
    let hidden = wrapped.len().saturating_sub(STREAM_EXPANDED_LINE_CAP);
    if hidden > 0 {
        wrapped.truncate(STREAM_EXPANDED_LINE_CAP);
    }
    lines.extend(prefix_lines(
        wrapped,
        indent_span.clone(),
        indent_span.clone(),
    ));
    if hidden > 0 {
        lines.push(
            format!(
                "{STREAM_SECTION_INDENT}… +{hidden} additional wrapped {} (open the process manager with Ctrl+Shift+B → `o` for the full log)",
                pluralize_line_word(hidden)
            )
            .dim()
            .into(),
        );
    }
    lines.push(stream_hint_line(kind, collapsed));
    lines
}

fn stream_hint_line(kind: ExecStreamKind, collapsed: bool) -> Line<'static> {
    let verb = if collapsed { "show" } else { "hide" };
    let action = if collapsed { "expand" } else { "collapse" };
    format!(
        "{STREAM_SECTION_INDENT}Use `/logs {} {verb}` to {action} this block.",
        kind.as_str()
    )
    .dim()
    .into()
}

fn colorize_stream_line(kind: ExecStreamKind, line: &str) -> Line<'static> {
    let mut styled = ansi_escape_line(line);
    let style = stream_style(kind);
    styled
        .spans
        .iter_mut()
        .for_each(|span| span.style = span.style.patch(style));
    styled
}

fn stream_style(kind: ExecStreamKind) -> Style {
    match kind {
        ExecStreamKind::Stdout => Style::default().fg(Color::Green),
        ExecStreamKind::Stderr => Style::default().fg(Color::Red),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_to_strings(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn collapsed_stream_includes_hint() {
        let lines = stream_section_lines(
            80,
            ExecStreamKind::Stdout,
            "alpha\nbeta\ngamma\ndelta",
            true,
        );
        let rendered = lines_to_strings(&lines);
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("/logs stdout show")),
            "expected collapsed hint"
        );
    }

    #[test]
    fn expanded_stream_includes_hint() {
        let lines = stream_section_lines(80, ExecStreamKind::Stdout, "alpha\nbeta\n", false);
        let rendered = lines_to_strings(&lines);
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("/logs stdout hide")),
            "expected expanded hint"
        );
    }
}

fn summarize_unknown_command(cmd: &str) -> Option<(&'static str, Vec<Span<'static>>)> {
    let meta = heredoc::analyze(cmd)?;
    let summary = heredoc_summary_from_metadata(&meta);
    Some((summary.label.as_str(), summary.detail_spans))
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

fn pluralize_line_word(count: usize) -> &'static str {
    if count == 1 { "line" } else { "lines" }
}

fn pluralize_lines(count: usize) -> Option<String> {
    if count == 0 {
        None
    } else {
        Some(format!("{count} {}", pluralize_line_word(count)))
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
