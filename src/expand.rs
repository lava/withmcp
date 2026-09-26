use anyhow::{Context, Result, bail};

use crate::config::{OAuth, Server};

/// Replaces every `${NAME}` in `input` with `lookup(NAME)`.
pub fn expand(input: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            bail!("unterminated `${{` in `{input}`");
        };
        let name = &after[..end];
        let value = lookup(name).with_context(|| format!("environment variable `{name}` is not set"))?;
        out.push_str(&value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

pub fn expand_server(server: &Server, lookup: &dyn Fn(&str) -> Option<String>) -> Result<Server> {
    let map = |values: &std::collections::BTreeMap<String, String>| {
        values
            .iter()
            .map(|(k, v)| Ok((k.clone(), expand(v, lookup)?)))
            .collect::<Result<_>>()
    };
    Ok(match server {
        Server::Stdio { command, args, env } => Server::Stdio {
            command: expand(command, lookup)?,
            args: args.iter().map(|a| expand(a, lookup)).collect::<Result<_>>()?,
            env: map(env)?,
        },
        Server::Http { url, headers, oauth } => Server::Http {
            url: expand(url, lookup)?,
            headers: map(headers)?,
            oauth: match oauth {
                Some(oauth) => Some(OAuth {
                    client_id: expand(&oauth.client_id, lookup)?,
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

    fn lookup(name: &str) -> Option<String> {
        (name == "TOKEN").then(|| "secret".to_string())
    }

    #[test]
    fn expands_variables() {
        assert_eq!(expand("Bearer ${TOKEN}", &lookup).unwrap(), "Bearer secret");
        assert_eq!(expand("${TOKEN}${TOKEN}", &lookup).unwrap(), "secretsecret");
        assert_eq!(expand("$TOKEN and $", &lookup).unwrap(), "$TOKEN and $");
    }

    #[test]
    fn errors() {
        assert!(expand("${MISSING}", &lookup).unwrap_err().to_string().contains("MISSING"));
        assert!(expand("${TOKEN", &lookup).is_err());
    }
}
