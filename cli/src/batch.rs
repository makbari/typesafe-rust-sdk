use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use clap::Args;
use serde_json::{Value, json};
use tokio::task::JoinSet;
use typesafe_sdk::{Client, Question, SystemOneRequest};

use crate::{ConnArgs, QuestionArgs};

#[derive(Args)]
pub struct BatchArgs {
    /// Input file with one record per line ("-" for stdin).
    #[arg(value_name = "INPUT", default_value = "-")]
    input: PathBuf,

    #[command(flatten)]
    questions: QuestionArgs,

    #[command(flatten)]
    conn: ConnArgs,

    /// Write results here instead of stdout. Refuses to overwrite unless --resume is given.
    #[arg(short, long, value_name = "PATH")]
    out: Option<PathBuf>,

    /// Requests in flight at once.
    #[arg(short, long, default_value_t = 4, value_parser = clap::value_parser!(u16).range(1..=64))]
    concurrency: u16,

    /// Read the text from this field of each JSON object.
    #[arg(long, value_name = "NAME", conflicts_with = "text")]
    field: Option<String>,

    /// Treat each line as plain text instead of JSON.
    #[arg(long)]
    text: bool,

    /// Continue a previous run: keep the complete lines already in --out, skip those inputs.
    #[arg(long, requires = "out")]
    resume: bool,
}

/// Turn one input line into the request state.
fn parse_record(line: &str, field: Option<&str>, text: bool) -> Result<Value, String> {
    if text {
        return Ok(Value::String(line.to_owned()));
    }
    let value: Value = serde_json::from_str(line).map_err(|e| format!("invalid JSON: {e}"))?;
    match field {
        None => Ok(value),
        Some(name) => value
            .get(name)
            .cloned()
            .ok_or_else(|| format!("missing field \"{name}\"")),
    }
}

/// One output line: the result, or the error, next to the input it belongs to.
fn output_record(line_no: usize, input: &Value, result: Result<Value, String>) -> String {
    let mut record = json!({ "line": line_no, "input": input });
    match result {
        Ok(Value::Object(fields)) => record.as_object_mut().unwrap().extend(fields),
        Ok(other) => record["result"] = other,
        Err(message) => record["error"] = Value::String(message),
    }
    record.to_string()
}

/// Number of complete (newline-terminated) lines in `bytes`, and their byte length.
fn complete_prefix(bytes: &[u8]) -> (usize, usize) {
    let lines = bytes.iter().filter(|b| **b == b'\n').count();
    let len = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    (lines, len)
}

/// Open the output. Returns the writer and how many records a previous run finished.
fn open_out(out: Option<&Path>, resume: bool) -> Result<(Box<dyn Write>, usize), String> {
    let Some(path) = out else {
        return Ok((Box::new(std::io::stdout()), 0));
    };
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    if !resume {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    format!(
                        "{} already exists; use --resume to continue it, or remove it",
                        path.display()
                    )
                } else {
                    err(e)
                }
            })?;
        return Ok((Box::new(file), 0));
    }
    let existing = std::fs::read(path).unwrap_or_default();
    let (done, keep) = complete_prefix(&existing);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(err)?;
    // Drop a half-written last line left by a crash.
    file.set_len(keep as u64).map_err(err)?;
    Ok((Box::new(file), done))
}

fn open_input(path: &Path) -> Result<Box<dyn BufRead>, String> {
    if path.as_os_str() == "-" {
        return Ok(Box::new(BufReader::new(std::io::stdin())));
    }
    let file = File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(Box::new(BufReader::new(file)))
}

struct Sink {
    out: Box<dyn Write>,
    pending: BTreeMap<usize, (String, bool)>,
    next: usize,
    ok: usize,
    failed: usize,
}

impl Sink {
    /// Accept a finished record, then write every record that is now next in input order.
    fn push(&mut self, index: usize, text: String, ok: bool) -> Result<(), String> {
        self.pending.insert(index, (text, ok));
        while let Some((text, ok)) = self.pending.remove(&self.next) {
            writeln!(self.out, "{text}")
                .and_then(|()| self.out.flush())
                .map_err(|e| format!("cannot write output: {e}"))?;
            self.next += 1;
            if ok { self.ok += 1 } else { self.failed += 1 }
        }
        Ok(())
    }
}

async fn run_one(
    client: Client,
    questions: Vec<(String, Question)>,
    model: Option<String>,
    line_no: usize,
    line: String,
    field: Option<String>,
    text: bool,
) -> (String, bool) {
    let state = match parse_record(&line, field.as_deref(), text) {
        Ok(state) => state,
        Err(message) => {
            return (
                output_record(line_no, &Value::String(line), Err(message)),
                false,
            );
        }
    };
    let mut request = SystemOneRequest::new(state.clone());
    if let Some(model) = model {
        request = request.model(model);
    }
    for (name, question) in questions {
        request = request.question(name, question);
    }
    let result = match client.system_one(request).await {
        Ok(r) => serde_json::to_value(r).map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    let ok = result.is_ok();
    (output_record(line_no, &state, result), ok)
}

/// Returns the number of failed records.
pub async fn run(args: BatchArgs) -> Result<usize, String> {
    let questions = args.questions.questions()?;
    if questions.is_empty() {
        return Err("no questions: use --noul, --choice, or --questions".into());
    }
    let client = args.conn.client()?;
    let input = open_input(&args.input)?;
    let (out, skip) = open_out(args.out.as_deref(), args.resume)?;
    let mut sink = Sink {
        out,
        pending: BTreeMap::new(),
        next: skip,
        ok: 0,
        failed: 0,
    };
    let limit = usize::from(args.concurrency);
    let mut tasks: JoinSet<(usize, String, bool)> = JoinSet::new();

    let mut index = 0;
    for (n, line) in input.lines().enumerate() {
        let line = line.map_err(|e| format!("cannot read input: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let i = index;
        index += 1;
        if i < skip {
            continue;
        }
        while tasks.len() >= limit {
            drain_one(&mut tasks, &mut sink).await?;
        }
        let (client, questions, model) =
            (client.clone(), questions.clone(), args.conn.model.clone());
        let (field, text) = (args.field.clone(), args.text);
        tasks.spawn(async move {
            let (record, ok) = run_one(client, questions, model, n + 1, line, field, text).await;
            (i, record, ok)
        });
    }
    while !tasks.is_empty() {
        drain_one(&mut tasks, &mut sink).await?;
    }
    eprintln!(
        "done: {} ok, {} failed{}",
        sink.ok,
        sink.failed,
        if skip > 0 {
            format!(", {skip} skipped (resume)")
        } else {
            String::new()
        }
    );
    Ok(sink.failed)
}

async fn drain_one(
    tasks: &mut JoinSet<(usize, String, bool)>,
    sink: &mut Sink,
) -> Result<(), String> {
    match tasks.join_next().await {
        Some(Ok((i, text, ok))) => sink.push(i, text, ok),
        Some(Err(e)) => Err(format!("worker failed: {e}")),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_records() {
        assert_eq!(parse_record("\"hi\"", None, false).unwrap(), json!("hi"));
        assert_eq!(
            parse_record(r#"{"body":"x","id":1}"#, Some("body"), false).unwrap(),
            json!("x")
        );
        assert!(
            parse_record(r#"{"id":1}"#, Some("body"), false)
                .unwrap_err()
                .contains("missing field")
        );
        assert!(
            parse_record("not json", None, false)
                .unwrap_err()
                .contains("invalid JSON")
        );
        assert_eq!(
            parse_record("not json", None, true).unwrap(),
            json!("not json")
        );
    }

    #[test]
    fn formats_output_records() {
        let ok = output_record(3, &json!("hi"), Ok(json!({"model": "m", "answers": {}})));
        assert_eq!(
            serde_json::from_str::<Value>(&ok).unwrap(),
            json!({"line": 3, "input": "hi", "model": "m", "answers": {}})
        );
        let bad = output_record(4, &json!("x"), Err("boom".into()));
        assert_eq!(
            serde_json::from_str::<Value>(&bad).unwrap(),
            json!({"line": 4, "input": "x", "error": "boom"})
        );
    }

    #[test]
    fn finds_complete_prefix() {
        assert_eq!(complete_prefix(b""), (0, 0));
        assert_eq!(complete_prefix(b"a\nb\n"), (2, 4));
        assert_eq!(complete_prefix(b"a\nb\nhalf"), (2, 4));
        assert_eq!(complete_prefix(b"half"), (0, 0));
    }

    #[test]
    fn sink_writes_in_input_order() {
        let dir = std::env::temp_dir().join(format!("tsb-{}", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        let (out, _) = open_out(Some(&dir), false).unwrap();
        let mut sink = Sink {
            out,
            pending: BTreeMap::new(),
            next: 0,
            ok: 0,
            failed: 0,
        };
        sink.push(2, "c".into(), true).unwrap();
        sink.push(0, "a".into(), true).unwrap();
        assert_eq!(std::fs::read_to_string(&dir).unwrap(), "a\n");
        sink.push(1, "b".into(), false).unwrap();
        assert_eq!(std::fs::read_to_string(&dir).unwrap(), "a\nb\nc\n");
        assert_eq!((sink.ok, sink.failed), (2, 1));
        assert!(open_out(Some(&dir), false).is_err()); // refuses to overwrite
        std::fs::remove_file(&dir).unwrap();
    }
}
