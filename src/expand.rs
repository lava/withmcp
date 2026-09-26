use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::config::{OAuth, Server};

/// Where the values of `${NAME}` and `$(command)` come from.
pub struct Sources<'a> {
    pub var: &'a dyn Fn(&str) -> Option<String>,
    /// Runs a program with arguments and returns its stdout.
    pub run: &'a dyn Fn(&str, &[&str]) -> Result<String>,
}

impl Sources<'_> {
    pub fn system() -> Sources<'static> {
        Sources {
            var: &|name| std::env::var(name).ok(),
            run: &run,
        }
    }
}

/// Runs `program` without a shell, capturing stdout and stderr.
fn run(program: &str, args: &[&str]) -> Result<String> {
    let output = match Command::new(program)
        .args(args)
        .stdin(Stdio::inherit())
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!("program `{program}` not found")
        }
        Err(err) => return Err(err).with_context(|| format!("cannot run `{program}`")),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim_end();
        if stderr.is_empty() {
            bail!("failed ({})", output.status);
        }
        bail!("failed ({}):\n{stderr}", output.status);
    }
    String::from_utf8(output.stdout).context("output is not valid UTF-8")
}

/// Replaces every `${NAME}` in `input` with the variable's value and every
/// `$(program arg...)` with the single line the program prints. Arguments
/// are split on whitespace; there is no shell.
pub fn expand(input: &str, sources: &Sources) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${").into_iter().chain(rest.find("$(")).min() {
        out.push_str(&rest[..start]);
        let (open, close) = if rest[start..].starts_with("${") {
            ("${", '}')
        } else {
            ("$(", ')')
        };
        let after = &rest[start + 2..];
        let Some(end) = after.find(close) else {
            bail!("unterminated `{open}` in `{input}`");
        };
        let inner = &after[..end];
        if close == '}' {
            out.push_str(
                &(sources.var)(inner)
                    .with_context(|| format!("environment variable `{inner}` is not set"))?,
            );
        } else {
            out.push_str(
                &substitute(inner, sources)
                    .with_context(|| format!("command `{}`", inner.trim()))?,
            );
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn substitute(command: &str, sources: &Sources) -> Result<String> {
    let mut words = command.split_whitespace();
    let Some(program) = words.next() else {
        bail!("is empty");
    };
    let output = (sources.run)(program, &words.collect::<Vec<_>>())?;
    let value = output.trim_end_matches(['\n', '\r']);
    if value.is_empty() {
        bail!("printed nothing");
    }
    if value.contains('\n') {
        bail!("printed more than one line");
    }
    Ok(value.to_string())
}

pub fn expand_server(server: &Server, sources: &Sources) -> Result<Server> {
    let map = |values: &std::collections::BTreeMap<String, String>| {
        values
            .iter()
            .map(|(k, v)| Ok((k.clone(), expand(v, sources)?)))
            .collect::<Result<_>>()
    };
    Ok(match server {
        Server::Stdio { command, args, env } => Server::Stdio {
            command: expand(command, sources)?,
            args: args
                .iter()
                .map(|a| expand(a, sources))
                .collect::<Result<_>>()?,
            env: map(env)?,
        },
        Server::Http {
            url,
            headers,
            oauth,
        } => Server::Http {
            url: expand(url, sources)?,
            headers: map(headers)?,
            oauth: match oauth {
                Some(oauth) => Some(OAuth {
                    client_id: expand(&oauth.client_id, sources)?,
                    callback_port: oauth.callback_port,
                }),
                None => None,
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(name: &str) -> Option<String> {
        (name == "TOKEN").then(|| "secret".to_string())
    }

    fn run(program: &str, args: &[&str]) -> Result<String> {
        match (program, args) {
            ("echo", args) => Ok(format!("{}\n", args.join(" "))),
            ("lines", _) => Ok("a\nb\n".into()),
            ("silent", _) => Ok("\n".into()),
            _ => bail!("program `{program}` not found"),
        }
    }

    fn expand(input: &str) -> Result<String> {
        super::expand(
            input,
            &Sources {
                var: &var,
                run: &run,
            },
        )
    }

    fn error(input: &str) -> String {
        format!("{:#}", expand(input).unwrap_err())
    }

    #[test]
    fn expands_variables() {
        assert_eq!(expand("Bearer ${TOKEN}").unwrap(), "Bearer secret");
        assert_eq!(expand("${TOKEN}${TOKEN}").unwrap(), "secretsecret");
        assert_eq!(expand("$TOKEN and $").unwrap(), "$TOKEN and $");
    }

    #[test]
    fn expands_commands() {
        assert_eq!(expand("Bearer $( echo  a   b )").unwrap(), "Bearer a b");
        assert_eq!(
            expand("$(echo ${TOKEN})-${TOKEN}").unwrap(),
            "${TOKEN}-secret"
        );
        assert_eq!(expand("$(echo a)b)").unwrap(), "ab)");
    }

    #[test]
    fn errors() {
        assert!(error("${MISSING}").contains("MISSING"));
        assert!(expand("${TOKEN").is_err());
        assert_eq!(error("x $(echo"), "unterminated `$(` in `x $(echo`");
        assert_eq!(error("$( )"), "command ``: is empty");
        assert_eq!(
            error("$(nope a)"),
            "command `nope a`: program `nope` not found"
        );
        assert_eq!(error("$(silent)"), "command `silent`: printed nothing");
        assert_eq!(
            error("$(lines)"),
            "command `lines`: printed more than one line"
        );
    }

    #[test]
    fn runs_programs() {
        assert_eq!(super::run("printf", &["x\\n"]).unwrap(), "x\n");
        assert_eq!(
            format!(
                "{:#}",
                super::run("withmcp-no-such-program", &[]).unwrap_err()
            ),
            "program `withmcp-no-such-program` not found"
        );
        let err = super::run("sh", &["-c", "echo oops >&2; exit 3"]).unwrap_err();
        assert_eq!(format!("{err:#}"), "failed (exit status: 3):\noops");
    }
}
