use rusm_node::ClientCommand;

/// A parsed line of REPL input from `rusm attach`.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplInput {
    Command(ClientCommand),
    Help,
    Quit,
    Empty,
    Unknown(String),
}

pub fn parse(line: &str) -> ReplInput {
    let mut parts = line.split_whitespace();
    let Some(verb) = parts.next() else {
        return ReplInput::Empty;
    };
    match verb {
        "help" | "?" => ReplInput::Help,
        "quit" | "exit" | "q" => ReplInput::Quit,
        "detail" => match parts.next() {
            Some("on") => ReplInput::Command(ClientCommand::SetDetail { enabled: true }),
            Some("off") => ReplInput::Command(ClientCommand::SetDetail { enabled: false }),
            _ => ReplInput::Unknown("usage: detail on|off".to_string()),
        },
        other => ReplInput::Unknown(format!("unknown command: {other}")),
    }
}

pub const HELP: &str = "\
commands:
  detail on|off    toggle the per-process detail table in snapshots
  help             show this help
  quit             leave the REPL";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_lines_are_empty() {
        assert_eq!(parse(""), ReplInput::Empty);
        assert_eq!(parse("   "), ReplInput::Empty);
    }

    #[test]
    fn help_and_quit_aliases() {
        for s in ["help", "?"] {
            assert_eq!(parse(s), ReplInput::Help);
        }
        for s in ["quit", "exit", "q"] {
            assert_eq!(parse(s), ReplInput::Quit);
        }
    }

    #[test]
    fn detail_on_off_and_misuse() {
        assert_eq!(
            parse("detail on"),
            ReplInput::Command(ClientCommand::SetDetail { enabled: true })
        );
        assert_eq!(
            parse("detail off"),
            ReplInput::Command(ClientCommand::SetDetail { enabled: false })
        );
        let usage = ReplInput::Unknown("usage: detail on|off".to_string());
        assert_eq!(parse("detail"), usage);
        assert_eq!(parse("detail maybe"), usage);
    }

    #[test]
    fn unknown_verbs_are_reported() {
        assert_eq!(
            parse("frobnicate"),
            ReplInput::Unknown("unknown command: frobnicate".to_string())
        );
    }

    #[test]
    fn extra_whitespace_is_tolerated() {
        assert_eq!(
            parse("  detail   on  "),
            ReplInput::Command(ClientCommand::SetDetail { enabled: true })
        );
    }
}
