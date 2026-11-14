//! `eid add|list|pause|resume`: thin remote control of the running app over the IPC endpoint.

use eid_ipc::{Client, ClientError, Command, Endpoint};
use std::path::PathBuf;

/// Which running instance to talk to.
#[derive(Debug, Default, PartialEq, Clone)]
pub struct ConnArgs {
    pub endpoint: Option<String>,
    pub token_file: Option<PathBuf>,
}

impl ConnArgs {
    pub fn endpoint(&self) -> Result<Endpoint, std::io::Error> {
        let name = self.endpoint.as_deref().unwrap_or("main");
        let mut ep = Endpoint::default_for(name).or_else(|e| match &self.token_file {
            Some(p) => Ok(Endpoint {
                name: name.to_owned(),
                token_path: p.clone(),
            }),
            None => Err(e),
        })?;
        if let Some(p) = &self.token_file {
            ep.token_path = p.clone();
        }
        Ok(ep)
    }
}

/// Consume `--endpoint`/`--token-file` if `flag` is one of them.
pub fn parse_conn_flag(
    c: &mut ConnArgs,
    flag: &str,
    it: &mut std::slice::Iter<String>,
) -> Result<bool, String> {
    match flag {
        "--endpoint" => c.endpoint = Some(it.next().ok_or("--endpoint needs a value")?.clone()),
        "--token-file" => {
            c.token_file = Some(PathBuf::from(
                it.next().ok_or("--token-file needs a value")?,
            ))
        }
        _ => return Ok(false),
    }
    Ok(true)
}

#[derive(Debug, PartialEq)]
pub enum Remote {
    Add {
        url: String,
        subdir: Option<String>,
        file_name: Option<String>,
    },
    List,
    Pause(u64),
    Resume(u64),
}

#[derive(Debug, PartialEq)]
pub struct RemoteArgs {
    pub conn: ConnArgs,
    pub what: Remote,
}

pub fn parse(argv: &[String]) -> Result<RemoteArgs, String> {
    let sub = argv.first().ok_or("missing command")?.as_str();
    let mut conn = ConnArgs::default();
    let (mut subdir, mut name) = (None, None);
    let mut pos: Vec<String> = vec![];
    let mut it = argv[1..].iter();
    while let Some(x) = it.next() {
        if parse_conn_flag(&mut conn, x, &mut it)? {
            continue;
        }
        match x.as_str() {
            "--subdir" if sub == "add" => {
                subdir = Some(it.next().ok_or("--subdir needs a value")?.clone())
            }
            "--name" if sub == "add" => {
                name = Some(it.next().ok_or("--name needs a value")?.clone())
            }
            "--" => pos.extend(it.by_ref().cloned()),
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option: {s}")),
            _ => pos.push(x.clone()),
        }
    }
    let one = |what: &str, pos: &[String]| -> Result<String, String> {
        match pos {
            [x] => Ok(x.clone()),
            [] => Err(format!("{sub} needs {what}")),
            _ => Err("unexpected extra argument".into()),
        }
    };
    let id = |s: String| {
        s.parse::<u64>()
            .map_err(|_| "item id must be a number".to_string())
    };
    let what = match sub {
        "add" => Remote::Add {
            url: one("a URL", &pos)?,
            subdir,
            file_name: name,
        },
        "list" => {
            if !pos.is_empty() {
                return Err("unexpected extra argument".into());
            }
            Remote::List
        }
        "pause" => Remote::Pause(id(one("an item id", &pos)?)?),
        "resume" => Remote::Resume(id(one("an item id", &pos)?)?),
        other => return Err(format!("unknown command: {other}")),
    };
    Ok(RemoteArgs { conn, what })
}

/// One request with at most one reconnect-and-retry **under the same request id**: if the
/// reply was lost the server answers from its idempotency cache instead of running it twice.
pub async fn call_retrying(ep: &Endpoint, cmd: Command) -> Result<serde_json::Value, String> {
    let id = eid_platform::ipc::new_token().map_err(|e| e.to_string())?[..32].to_owned();
    let mut last = None;
    for _ in 0..2 {
        let mut c = match Client::connect(ep).await {
            Ok(c) => c,
            Err(ClientError::NotRunning) => {
                return Err("Eid Download is not running (start it, or run `eid serve`)".into())
            }
            Err(e) => {
                last = Some(e);
                continue;
            }
        };
        match c.call_with_id(&id, cmd.clone()).await {
            Ok(v) => return Ok(v),
            Err(ClientError::Server(m)) => return Err(m),
            Err(e) => last = Some(e),
        }
    }
    Err(last
        .map(|e| e.to_string())
        .unwrap_or_else(|| "failed".into()))
}

/// Returns the exit code. Output goes to stdout; errors to the caller.
pub async fn run(a: RemoteArgs) -> Result<i32, String> {
    let ep = a.conn.endpoint().map_err(|e| e.to_string())?;
    match a.what {
        Remote::Add {
            url,
            subdir,
            file_name,
        } => {
            let r = call_retrying(
                &ep,
                Command::Add {
                    url,
                    subdir,
                    file_name,
                },
            )
            .await?;
            println!("added #{}", r["id"]);
        }
        Remote::List => {
            let r = call_retrying(&ep, Command::List {}).await?;
            for i in r["items"].as_array().into_iter().flatten() {
                println!(
                    "#{} {} {}{}",
                    i["id"],
                    i["status"].as_str().unwrap_or("?"),
                    i["url"].as_str().unwrap_or("?"),
                    i["error"]
                        .as_str()
                        .map(|e| format!("  ({e})"))
                        .unwrap_or_default()
                );
            }
            let total = r["total"].as_u64().unwrap_or(0);
            let shown = r["items"].as_array().map_or(0, |a| a.len()) as u64;
            if total > shown {
                println!("... and {} more", total - shown);
            }
        }
        Remote::Pause(id) => {
            call_retrying(&ep, Command::Pause { item: id }).await?;
            println!("paused #{id}");
        }
        Remote::Resume(id) => {
            call_retrying(&ep, Command::Resume { item: id }).await?;
            println!("resumed #{id}");
        }
    }
    Ok(0)
}
