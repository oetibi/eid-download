//! Hand-rolled argument parsing (no dependency, trivially testable).

use std::path::PathBuf;

#[derive(Debug, PartialEq)]
pub enum Cmd {
    Get(GetArgs),
    Resume(ResumeArgs),
    Help,
    Version,
}

#[derive(Debug, PartialEq, Default, Clone)]
pub struct Common {
    pub connections: Option<usize>,
    pub limit_bps: Option<u64>,
    pub sha256: Option<String>,
    pub quiet: bool,
    // advanced / test knobs
    pub ramp_ms: Option<u64>,
    pub checkpoint_ms: Option<u64>,
    pub min_split: Option<u64>,
    pub stall_ms: Option<u64>,
    pub attempts: Option<u32>,
}

#[derive(Debug, PartialEq, Default)]
pub struct GetArgs {
    pub url: String,
    pub dir: Option<PathBuf>,
    pub name: Option<String>,
    pub common: Common,
}

#[derive(Debug, PartialEq)]
pub struct ResumeArgs {
    pub state: PathBuf,
    pub common: Common,
}

/// `500`, `64K`, `2M`, `1.5M`, `1G` (binary multiples) => bytes per second.
pub fn parse_rate(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('k' | 'K') => (&s[..s.len() - 1], 1024.0),
        Some('m' | 'M') => (&s[..s.len() - 1], 1024.0 * 1024.0),
        Some('g' | 'G') => (&s[..s.len() - 1], 1024.0 * 1024.0 * 1024.0),
        _ => (s, 1.0),
    };
    let v: f64 = num.trim().parse().map_err(|_| format!("bad rate: {s}"))?;
    if !v.is_finite() || v <= 0.0 || v * mult > 1e13 {
        return Err(format!("rate out of range: {s}"));
    }
    Ok((v * mult) as u64)
}

pub fn parse(args: &[String]) -> Result<Cmd, String> {
    let mut it = args.iter().peekable();
    let Some(sub) = it.next() else {
        return Ok(Cmd::Help);
    };
    match sub.as_str() {
        "-h" | "--help" | "help" => return Ok(Cmd::Help),
        "-V" | "--version" => return Ok(Cmd::Version),
        _ => {}
    }
    let mut common = Common::default();
    let mut positional: Option<String> = None;
    let mut dir = None;
    let mut name = None;
    let num = |it: &mut std::iter::Peekable<std::slice::Iter<String>>,
               flag: &str|
     -> Result<u64, String> {
        it.next()
            .ok_or(format!("{flag} needs a value"))?
            .parse::<u64>()
            .map_err(|_| format!("{flag}: not a number"))
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "-d" | "--dir" => dir = Some(PathBuf::from(it.next().ok_or("-d needs a value")?)),
            "-o" | "--output" => name = Some(it.next().ok_or("-o needs a value")?.clone()),
            "-c" | "--connections" => {
                let n = num(&mut it, "-c")?;
                if !(1..=32).contains(&n) {
                    return Err("-c must be between 1 and 32".into());
                }
                common.connections = Some(n as usize);
            }
            "--limit" => {
                common.limit_bps = Some(parse_rate(it.next().ok_or("--limit needs a value")?)?)
            }
            "--sha256" => {
                let h = it.next().ok_or("--sha256 needs a value")?;
                if h.len() != 64 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err("--sha256 must be 64 hex characters".into());
                }
                common.sha256 = Some(h.to_ascii_lowercase());
            }
            "-q" | "--quiet" => common.quiet = true,
            "--ramp-ms" => common.ramp_ms = Some(num(&mut it, a)?),
            "--checkpoint-ms" => common.checkpoint_ms = Some(num(&mut it, a)?),
            "--min-split" => common.min_split = Some(num(&mut it, a)?),
            "--stall-ms" => common.stall_ms = Some(num(&mut it, a)?),
            "--attempts" => common.attempts = Some(num(&mut it, a)? as u32),
            "--" => {
                positional = it.next().cloned().or(positional);
            }
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option: {s}")),
            _ => {
                if positional.is_some() {
                    return Err("unexpected extra argument".into());
                }
                positional = Some(a.clone());
            }
        }
    }
    match sub.as_str() {
        "get" => Ok(Cmd::Get(GetArgs {
            url: positional.ok_or("get needs a URL")?,
            dir,
            name,
            common,
        })),
        "resume" => {
            if dir.is_some() || name.is_some() {
                return Err("resume takes only the state file".into());
            }
            Ok(Cmd::Resume(ResumeArgs {
                state: PathBuf::from(positional.ok_or("resume needs a state file")?),
                common,
            }))
        }
        other => Err(format!("unknown command: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn rates() {
        assert_eq!(parse_rate("64K").unwrap(), 65536);
        assert_eq!(parse_rate("1.5M").unwrap(), 1572864);
        assert!(parse_rate("0").is_err());
        assert!(parse_rate("-3").is_err());
        assert!(parse_rate("inf").is_err());
        assert!(parse_rate("abc").is_err());
    }
    #[test]
    fn get_parses() {
        let c = parse(&v(&[
            "get",
            "http://x/a",
            "-c",
            "8",
            "--limit",
            "2M",
            "-d",
            "out",
        ]))
        .unwrap();
        match c {
            Cmd::Get(g) => {
                assert_eq!(g.url, "http://x/a");
                assert_eq!(g.common.connections, Some(8));
                assert_eq!(g.common.limit_bps, Some(2 * 1024 * 1024));
            }
            _ => panic!(),
        }
        assert!(parse(&v(&["get", "u", "-c", "33"])).is_err());
        assert!(parse(&v(&["get"])).is_err());
        assert!(parse(&v(&["get", "a", "b"])).is_err());
    }
}
