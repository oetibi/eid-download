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
    Stop,
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
        "stop" => {
            if !pos.is_empty() {
                return Err("unexpected extra argument".into());
            }
            Remote::Stop
        }
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
        Remote::Stop => {
            call_retrying(&ep, Command::Shutdown {}).await?;
            println!("stopping");
        }
        Remote::Resume(id) => {
            call_retrying(&ep, Command::Resume { item: id }).await?;
            println!("resumed #{id}");
        }
    }
    Ok(0)
}

/// Does `argv` (without the program name) belong to `eid serve` / the remote commands?
///
/// `eid resume` exists twice: `eid resume <file.eid-state>` (P1, standalone engine) and
/// `eid resume <item-id>` (P3, the running app). It is the remote one only when its first
/// argument is a plain number or a connection flag; a state file path is never all digits and
/// never one of those flags.
pub fn is_app_command(argv: &[String]) -> bool {
    match argv.first().map(String::as_str) {
        Some("serve" | "add" | "list" | "pause" | "stop") => true,
        Some("resume") => match argv.get(1).map(String::as_str) {
            Some(a) => {
                a == "--endpoint"
                    || a == "--token-file"
                    || (!a.is_empty() && a.bytes().all(|b| b.is_ascii_digit()))
            }
            None => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn resume_is_routed_to_the_app_only_for_an_item_id_or_a_connection_flag() {
        for remote in [
            v(&["serve"]),
            v(&["add", "http://h/x"]),
            v(&["list"]),
            v(&["pause", "1"]),
            v(&["stop"]),
            v(&["resume", "12"]),
            v(&["resume", "--endpoint", "x", "3"]),
            v(&["resume", "--token-file", "t", "3"]),
        ] {
            assert!(is_app_command(&remote), "{remote:?}");
        }
        for legacy in [
            v(&["resume", "file.eid-state"]),
            v(&["resume", r"C:\dl\a.bin.eid-state", "-q"]),
            v(&["resume", "-q", "x.eid-state"]),
            v(&["resume", "12.eid-state"]),
            v(&["resume"]),
            v(&["get", "http://h/x"]),
            v(&["queue", "list", "q.json"]),
            v(&[]),
        ] {
            assert!(!is_app_command(&legacy), "{legacy:?}");
        }
    }

    #[test]
    fn remote_args_parse_and_reject() {
        let a = parse(&v(&[
            "add",
            "http://h/a",
            "--subdir",
            "s",
            "--name",
            "n.bin",
        ]))
        .unwrap();
        assert!(matches!(
            a.what,
            Remote::Add {
                subdir: Some(_),
                file_name: Some(_),
                ..
            }
        ));
        assert_eq!(parse(&v(&["pause", "7"])).unwrap().what, Remote::Pause(7));
        assert_eq!(parse(&v(&["resume", "7"])).unwrap().what, Remote::Resume(7));
        assert_eq!(parse(&v(&["stop"])).unwrap().what, Remote::Stop);
        for bad in [
            v(&["pause"]),
            v(&["pause", "x"]),
            v(&["pause", "-1"]),
            v(&["pause", "1", "2"]),
            v(&["add"]),
            v(&["add", "a", "b"]),
            v(&["list", "x"]),
            v(&["stop", "x"]),
            v(&["list", "--weird"]),
            v(&["list", "--endpoint"]),
            v(&["pause", "1", "--subdir", "x"]),
        ] {
            assert!(parse(&bad).is_err(), "{bad:?}");
        }
    }
}
