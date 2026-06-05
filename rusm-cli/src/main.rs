use futures_util::{SinkExt, StreamExt};
use rusm_bench::{serve, ClientCommand, Node, RunnerConfig};
use rusm_cli::{normalize_target, parse, render_message, ReplInput, DEFAULT_HOST, HELP};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_tungstenite::tungstenite::Message;

const DEFAULT_LISTEN: &str = "127.0.0.1:4000";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(String::as_str);
    let subcommand = args.get(2).map(String::as_str);

    if command == Some("node") && subcommand == Some("start") {
        let addr = flag(&args, "--listen").unwrap_or_else(|| DEFAULT_LISTEN.to_string());
        let node = Node::new(RunnerConfig::default());
        println!("rusm node listening on ws://{addr}");
        if let Err(error) = serve(&addr, node).await {
            eprintln!("node error: {error}");
            std::process::exit(1);
        }
    } else if command == Some("attach") {
        // Target defaults to the local node and accepts host / host:port / ws-url.
        let target = normalize_target(args.get(2).map(String::as_str).unwrap_or(DEFAULT_HOST));
        if let Err(error) = attach(&target).await {
            eprintln!("attach failed: {error}");
            std::process::exit(1);
        }
    } else {
        eprintln!("usage:");
        eprintln!("  rusm node start [--listen <addr>]");
        eprintln!("  rusm attach [<host | host:port | ws-url>]   (defaults to 127.0.0.1:4000)");
        std::process::exit(2);
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let idx = args.iter().position(|a| a == name)?;
    args.get(idx + 1).cloned()
}

async fn attach(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (ws, _) = tokio_tungstenite::connect_async(url).await?;
    let (mut write, mut read) = ws.split();
    println!("attached to {url} — type `help` for commands");

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            incoming = read.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if let Ok(message) = rusm_bench::ServerMessage::from_json(text.as_str()) {
                        println!("{}", render_message(&message));
                    }
                }
                Some(Ok(Message::Close(_))) | None => {
                    println!("node disconnected");
                    break;
                }
                _ => {}
            },
            line = lines.next_line() => match line {
                Ok(Some(line)) => match parse(&line) {
                    ReplInput::Command(cmd) => send(&mut write, &cmd).await?,
                    ReplInput::Help => println!("{HELP}"),
                    ReplInput::Quit => break,
                    ReplInput::Empty => {}
                    ReplInput::Unknown(msg) => println!("{msg}"),
                },
                _ => break,
            },
        }
    }
    Ok(())
}

async fn send<S>(write: &mut S, command: &ClientCommand) -> Result<(), Box<dyn std::error::Error>>
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::error::Error + 'static,
{
    write.send(Message::Text(command.to_json().into())).await?;
    Ok(())
}
