mod batch;
mod logger;

use std::{
    io::{IsTerminal, Read},
    path::PathBuf,
    process::ExitCode,
};

use clap::{Args, Parser, Subcommand};
use serde_json::Value;
use typesafe_sdk::{Answer, Client, Question, SystemOneRequest, SystemOneResult, choice, noul};

use batch::BatchArgs;

#[derive(Parser)]
#[command(
    name = "typesafe",
    version,
    about = "Command-line client for the TypeSafe AI API"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Ask questions about a piece of text and print the answers as JSON.
    ///
    /// The text comes from STATE, from --file, or from stdin. The API key is read from
    /// TYPESAFE_API_KEY.
    Ask(AskArgs),
    /// Answer the same questions for every line of a JSONL (or text) file.
    ///
    /// Results are written in input order, one JSON object per line, with the input beside
    /// each answer. Exit code 1 means some records failed.
    Batch(BatchArgs),
}

#[derive(Args)]
struct AskArgs {
    /// Text to evaluate. Omit to read --file or stdin.
    state: Option<String>,

    /// Read the text from a file ("-" for stdin).
    #[arg(short, long, value_name = "PATH", conflicts_with = "state")]
    file: Option<PathBuf>,

    #[command(flatten)]
    questions: QuestionArgs,

    #[command(flatten)]
    conn: ConnArgs,

    /// Print aligned text instead of JSON.
    #[arg(long)]
    pretty: bool,
}

/// Where the questions come from. Combine the flags freely.
#[derive(Args)]
pub struct QuestionArgs {
    /// Yes/no question: NAME=INSTRUCTIONS (repeatable).
    #[arg(long, value_name = "NAME=INSTRUCTIONS", value_parser = parse_noul)]
    noul: Vec<(String, Question)>,

    /// Pick-one question: NAME[:INSTRUCTIONS]=LABEL,LABEL,... (repeatable).
    #[arg(long, value_name = "NAME[:INSTRUCTIONS]=LABELS", value_parser = parse_choice)]
    choice: Vec<(String, Question)>,

    /// JSON file mapping question names to questions in API format, for any question type.
    #[arg(short, long, value_name = "PATH")]
    questions: Option<PathBuf>,
}

impl QuestionArgs {
    pub fn questions(&self) -> Result<Vec<(String, Question)>, String> {
        let mut all = Vec::new();
        if let Some(path) = &self.questions {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let map: std::collections::BTreeMap<String, Question> = serde_json::from_str(&text)
                .map_err(|e| format!("invalid questions file {}: {e}", path.display()))?;
            all.extend(map);
        }
        all.extend(self.noul.iter().cloned().chain(self.choice.iter().cloned()));
        Ok(all)
    }
}

/// How to reach the API.
#[derive(Args)]
pub struct ConnArgs {
    /// Model to use. Default: TYPESAFE_DEFAULT_MODEL, then jev-latest.
    #[arg(short, long)]
    pub model: Option<String>,

    /// API root. Default: TYPESAFE_BASE_URL, then https://api.typesafe.ai.
    #[arg(long, value_name = "URL")]
    base_url: Option<String>,

    /// Timeout per attempt, in seconds.
    #[arg(long, value_name = "SECS")]
    timeout: Option<u64>,
}

impl ConnArgs {
    pub fn client(&self) -> Result<Client, String> {
        let mut builder = Client::builder();
        if let Some(url) = &self.base_url {
            builder = builder.base_url(url);
        }
        if let Some(secs) = self.timeout {
            builder = builder.timeout(std::time::Duration::from_secs(secs));
        }
        builder.build().map_err(|e| e.to_string())
    }
}

fn parse_noul(s: &str) -> Result<(String, Question), String> {
    let (name, instructions) = s.split_once('=').ok_or("expected NAME=INSTRUCTIONS")?;
    let name = non_empty(name, "question name")?;
    Ok((
        name.to_owned(),
        noul(non_empty(instructions, "instructions")?),
    ))
}

fn parse_choice(s: &str) -> Result<(String, Question), String> {
    // Split on the last '=' so instructions may contain '='.
    let (head, labels) = s
        .rsplit_once('=')
        .ok_or("expected NAME[:INSTRUCTIONS]=LABEL,LABEL")?;
    let (name, instructions) = head.split_once(':').unwrap_or((head, ""));
    let name = non_empty(name, "question name")?;
    let labels: Vec<&str> = labels
        .split(',')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if labels.is_empty() {
        return Err("at least one label is required".into());
    }
    let instructions = if instructions.is_empty() {
        name
    } else {
        instructions.trim()
    };
    Ok((
        name.to_owned(),
        choice(instructions, labels.into_iter().map(|l| (l, Value::Null))),
    ))
}

fn non_empty<'a>(s: &'a str, what: &str) -> Result<&'a str, String> {
    let s = s.trim();
    if s.is_empty() {
        Err(format!("{what} must not be empty"))
    } else {
        Ok(s)
    }
}

fn read_state(args: &AskArgs) -> Result<String, String> {
    let from_stdin = || -> Result<String, String> {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("cannot read stdin: {e}"))?;
        Ok(buf)
    };
    let text = match (&args.state, &args.file) {
        (Some(s), _) => s.clone(),
        (None, Some(p)) if p.as_os_str() == "-" => from_stdin()?,
        (None, Some(p)) => {
            std::fs::read_to_string(p).map_err(|e| format!("cannot read {}: {e}", p.display()))?
        }
        (None, None) if !std::io::stdin().is_terminal() => from_stdin()?,
        (None, None) => return Err("no text given: pass STATE, --file, or pipe stdin".into()),
    };
    let text = text.trim_end().to_owned();
    if text.is_empty() {
        Err("the text is empty".into())
    } else {
        Ok(text)
    }
}

/// One line per answer: `name  type  value`, names aligned.
fn render_pretty(result: &SystemOneResult) -> String {
    let width = result.answers.keys().map(String::len).max().unwrap_or(0);
    result
        .answers
        .iter()
        .map(|(name, answer)| {
            let (kind, value) = match answer {
                Answer::Noul(a) => ("noul", format!("{:.3}", a.noul)),
                Answer::Choice(a) => ("choice", format!("{} ({:.3})", a.choice, a.confidence)),
                Answer::Score(a) => ("score", format!("{:.2} ({:.3})", a.score, a.confidence)),
                Answer::Box(a) => (
                    "box",
                    format!(
                        "top {:.3} bottom {:.3} left {:.3} right {:.3}",
                        a.bounds.top, a.bounds.bottom, a.bounds.left, a.bounds.right
                    ),
                ),
            };
            format!("{name:<width$}  {kind:<6}  {value}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn ask(args: AskArgs) -> Result<(), String> {
    let state = read_state(&args)?;
    let client = args.conn.client()?;
    let mut request = SystemOneRequest::new(state);
    if let Some(model) = &args.conn.model {
        request = request.model(model);
    }
    for (name, question) in args.questions.questions()? {
        request = request.question(name, question);
    }
    let result = client
        .system_one(request)
        .await
        .map_err(|e| e.to_string())?;

    if args.pretty {
        println!("{}", render_pretty(&result));
    } else {
        println!(
            "{}",
            serde_json::to_string(&result).map_err(|e| e.to_string())?
        );
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Err(message) = logger::init() {
        eprintln!("error: {message}");
        return ExitCode::from(2);
    }
    let outcome = match cli.command {
        Command::Ask(args) => ask(args).await.map(|()| 0),
        Command::Batch(args) => batch::run(args).await,
    };
    match outcome {
        Ok(0) => ExitCode::SUCCESS,
        Ok(_) => ExitCode::from(1),
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;
    use serde_json::json;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_noul() {
        let (name, q) = parse_noul("urgent=Is this urgent? a=b").unwrap();
        assert_eq!(name, "urgent");
        assert_eq!(
            serde_json::to_value(q).unwrap(),
            json!({"type": "noul", "instructions": "Is this urgent? a=b"})
        );
        assert!(parse_noul("urgent").is_err());
        assert!(parse_noul("=x").is_err());
        assert!(parse_noul("x=").is_err());
    }

    #[test]
    fn parses_choice() {
        let (name, q) = parse_choice("cat:What is it?=billing, technical,other").unwrap();
        assert_eq!(name, "cat");
        assert_eq!(
            serde_json::to_value(q).unwrap(),
            json!({"type": "choice", "instructions": "What is it?", "criteria":
                {"billing": null, "technical": null, "other": null}})
        );
        // Without instructions the name stands in; '=' inside instructions is kept.
        let (_, q) = parse_choice("cat=a").unwrap();
        assert_eq!(serde_json::to_value(q).unwrap()["instructions"], "cat");
        let (_, q) = parse_choice("c:x=y?=a,b").unwrap();
        assert_eq!(serde_json::to_value(q).unwrap()["instructions"], "x=y?");
        assert!(parse_choice("cat").is_err());
        assert!(parse_choice("cat=,").is_err());
    }

    #[test]
    fn renders_aligned_lines() {
        let result: SystemOneResult = serde_json::from_value(json!({
            "model": "m",
            "answers": {
                "category": {"type": "choice", "choice": "billing", "confidence": 0.97, "probabilities": {"billing": 0.97}},
                "urgent": {"type": "noul", "noul": 0.12},
            },
            "usage": {"input_tokens": null, "output_tokens": null},
        }))
        .unwrap();
        assert_eq!(
            render_pretty(&result),
            "category  choice  billing (0.970)\nurgent    noul    0.120"
        );
    }
}
