use std::io::{self, IsTerminal, Write};

const MAX_BOX_WIDTH: usize = 92;

pub fn labeled(label: &str, value: impl AsRef<str>) -> String {
    format!("{label:<11}  {}", value.as_ref())
}

pub fn print_box(title: &str, lines: &[String]) {
    let width = output_width(io::stdout().is_terminal());
    let _ = write!(io::stdout(), "{}", box_text(title, lines, width));
}

pub fn print_error(error: &anyhow::Error) {
    let width = output_width(io::stderr().is_terminal());
    let _ = write!(
        io::stderr(),
        "{}",
        box_text("ERROR", &[error.to_string()], width)
    );
}

fn output_width(is_terminal: bool) -> usize {
    if !is_terminal {
        return MAX_BOX_WIDTH;
    }
    terminal_size::terminal_size()
        .map(|(terminal_size::Width(width), _)| usize::from(width).clamp(5, MAX_BOX_WIDTH))
        .unwrap_or(MAX_BOX_WIDTH)
}

pub fn box_text(title: &str, lines: &[String], width: usize) -> String {
    let width = width.max(5);
    let inner = width - 4;
    let mut output = String::new();
    output.push('╭');
    output.push_str(&"─".repeat(width - 2));
    output.push_str("╮\n");
    for line in wrap_line(&format!("RZ :: {title}"), inner) {
        push_box_line(&mut output, &line, inner);
    }
    output.push('├');
    output.push_str(&"─".repeat(width - 2));
    output.push_str("┤\n");
    for line in lines {
        for part in wrap_line(line, inner) {
            push_box_line(&mut output, &part, inner);
        }
    }
    output.push('╰');
    output.push_str(&"─".repeat(width - 2));
    output.push_str("╯\n");
    output
}

fn push_box_line(output: &mut String, text: &str, width: usize) {
    let length = text.chars().count();
    output.push_str("│ ");
    output.push_str(text);
    output.push_str(&" ".repeat(width.saturating_sub(length)));
    output.push_str(" │\n");
}

fn wrap_line(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let continuation = continuation_prefix(text, width);
    let mut remaining = text.trim_end().to_owned();
    let mut result = Vec::new();
    let mut first = true;
    while !remaining.is_empty() {
        let prefix = if first { "" } else { &continuation };
        let available = width.saturating_sub(prefix.chars().count()).max(1);
        if remaining.chars().count() <= available {
            result.push(format!("{prefix}{remaining}"));
            break;
        }
        let character_indices = remaining.char_indices().collect::<Vec<_>>();
        let byte_limit = character_indices
            .get(available)
            .map_or(remaining.len(), |(index, _)| *index);
        let candidate = &remaining[..byte_limit];
        let split = candidate
            .rfind(' ')
            .filter(|index| candidate[..*index].chars().count() >= available / 2)
            .unwrap_or(byte_limit);
        result.push(format!("{prefix}{}", remaining[..split].trim_end()));
        remaining = remaining[split..].trim_start().to_owned();
        first = false;
    }
    result
}

fn continuation_prefix(text: &str, width: usize) -> String {
    let label = text.split_once("  ").map(|(label, _)| label);
    let prefix = if label.is_some_and(|label| {
        !label.is_empty()
            && label.len() <= 11
            && label
                .chars()
                .all(|character| character == ' ' || character.is_ascii_uppercase())
    }) {
        " ".repeat(13)
    } else {
        text.chars()
            .take_while(|character| character.is_whitespace())
            .collect()
    };
    prefix.chars().take(width.saturating_sub(1)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_fits_narrow_and_wide_widths() {
        for width in [16, 40, 92] {
            let output = box_text(
                "WORKSPACE RESTORED",
                &["A line long enough to wrap inside a narrow terminal".into()],
                width,
            );
            assert!(output.lines().all(|line| line.chars().count() == width));
        }
    }

    #[test]
    fn labels_align_values() {
        assert_eq!(labeled("AMP", "T-123"), "AMP          T-123");
    }
}
