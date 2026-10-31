//! SMTP command line parsing (RFC 5321 §4.1.1, §4.1.2).

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Helo(String),
    Ehlo(String),
    Mail {
        path: Result<String, PathError>,
        params: Result<Vec<(String, Option<String>)>, PathError>,
    },
    Rcpt {
        path: Result<String, PathError>,
        params: Result<Vec<(String, Option<String>)>, PathError>,
    },
    Data,
    Rset,
    Noop,
    Quit,
    StartTls(String),
    Auth {
        mechanism: String,
        initial: Option<String>,
    },
    Vrfy,
    Expn,
    Help,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathError {
    /// Missing `FROM:`/`TO:` keyword or angle brackets.
    Syntax,
    /// Unquoted whitespace, control characters or a missing `@`.
    InvalidAddress,
    /// Longer than [`MAX_ADDRESS_LEN`].
    TooLong,
}

/// RFC 5321 §4.5.3.1 recommends 64+255; real clients send longer ones (1024 is seen in practice).
pub const MAX_ADDRESS_LEN: usize = 1024;

pub fn parse(line: &str) -> Command {
    let (verb, rest) = match line.find(' ') {
        Some(i) => (&line[..i], line[i + 1..].trim()),
        None => (line, ""),
    };
    match verb.to_ascii_uppercase().as_str() {
        "HELO" => Command::Helo(rest.to_string()),
        "EHLO" => Command::Ehlo(rest.to_string()),
        "MAIL" => {
            let (path, params) = parse_path_arg(rest, "FROM:", true);
            Command::Mail { path, params }
        }
        "RCPT" => {
            let (path, params) = parse_path_arg(rest, "TO:", false);
            Command::Rcpt { path, params }
        }
        "DATA" => Command::Data,
        "RSET" => Command::Rset,
        "NOOP" => Command::Noop,
        "QUIT" => Command::Quit,
        "STARTTLS" => Command::StartTls(rest.to_string()),
        "AUTH" => {
            let mut it = rest.split_whitespace();
            Command::Auth {
                mechanism: it.next().unwrap_or_default().to_ascii_uppercase(),
                initial: it.next().map(str::to_string),
            }
        }
        "VRFY" => Command::Vrfy,
        "EXPN" => Command::Expn,
        "HELP" => Command::Help,
        _ => Command::Unknown,
    }
}

type PathAndParams = (
    Result<String, PathError>,
    Result<Vec<(String, Option<String>)>, PathError>,
);

fn parse_path_arg(rest: &str, keyword: &str, allow_null: bool) -> PathAndParams {
    let Some(prefix) = rest.get(..keyword.len()) else {
        return (Err(PathError::Syntax), Ok(vec![]));
    };
    if !prefix.eq_ignore_ascii_case(keyword) {
        return (Err(PathError::Syntax), Ok(vec![]));
    }
    // One optional space after the colon is accepted ("FROM: <a@b>").
    let after = rest[keyword.len()..]
        .strip_prefix(' ')
        .unwrap_or(&rest[keyword.len()..]);
    let Some(inner_and_rest) = after.strip_prefix('<') else {
        return (Err(PathError::Syntax), Ok(vec![]));
    };
    let Some(end) = find_closing_bracket(inner_and_rest) else {
        return (Err(PathError::Syntax), Ok(vec![]));
    };
    let inner = &inner_and_rest[..end];
    let tail = &inner_and_rest[end + 1..];
    // Parameters must be separated from the path by a space (RFC 5321 §4.1.2).
    if !tail.is_empty() && !tail.starts_with(' ') {
        return (Err(PathError::Syntax), Ok(vec![]));
    }
    let path = validate_path(inner, allow_null);
    let params = parse_params(tail.trim());
    (path, params)
}

/// Index of the `>` closing the path, ignoring `>` inside a quoted local part.
fn find_closing_bracket(s: &str) -> Option<usize> {
    let (mut quoted, mut escaped) = (false, false);
    for (i, c) in s.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '>' if !quoted => return Some(i),
            _ => {}
        }
    }
    None
}

fn validate_path(inner: &str, allow_null: bool) -> Result<String, PathError> {
    if inner.is_empty() {
        return if allow_null {
            Ok(String::new())
        } else {
            Err(PathError::InvalidAddress)
        };
    }
    if inner.len() > MAX_ADDRESS_LEN {
        return Err(PathError::TooLong);
    }
    // Obsolete source route "@a,@b:user@c" (RFC 5321 §4.1.2, Appendix C): keep the mailbox.
    let mailbox = if inner.starts_with('@') {
        inner.split_once(':').map_or(inner, |(_, m)| m)
    } else {
        inner
    };
    if mailbox.chars().any(|c| c.is_control()) || has_unquoted_whitespace(mailbox) {
        return Err(PathError::InvalidAddress);
    }
    // RCPT TO:<Postmaster> without a domain is valid (RFC 5321 §4.1.1.3).
    if !allow_null && mailbox.eq_ignore_ascii_case("postmaster") {
        return Ok(mailbox.to_string());
    }
    match mailbox.rfind('@') {
        Some(at) if at > 0 && at + 1 < mailbox.len() => Ok(mailbox.to_string()),
        _ => Err(PathError::InvalidAddress),
    }
}

fn has_unquoted_whitespace(s: &str) -> bool {
    let (mut quoted, mut escaped) = (false, false);
    for c in s.chars() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ' ' | '\t' if !quoted => return true,
            _ => {}
        }
    }
    false
}

fn parse_params(s: &str) -> Result<Vec<(String, Option<String>)>, PathError> {
    s.split(' ')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = match p.split_once('=') {
                Some((k, v)) => (k, Some(v.to_string())),
                None => (p, None),
            };
            if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                return Err(PathError::Syntax);
            }
            Ok((k.to_ascii_uppercase(), v))
        })
        .collect()
}

/// RFC 4954 §5 / RFC 3461 xtext, or `<>`.
pub fn valid_auth_param(v: &str) -> bool {
    if v == "<>" {
        return true;
    }
    let b = v.as_bytes();
    if b.is_empty() {
        return false;
    }
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                if i + 2 >= b.len()
                    || !b[i + 1].is_ascii_hexdigit()
                    || !b[i + 2].is_ascii_hexdigit()
                {
                    return false;
                }
                i += 3;
            }
            33..=126 if b[i] != b'=' => i += 1,
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mail(line: &str) -> PathAndParams {
        match parse(line) {
            Command::Mail { path, params } | Command::Rcpt { path, params } => (path, params),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn verbs_are_case_insensitive() {
        assert_eq!(parse("ehlo x"), Command::Ehlo("x".into()));
        assert_eq!(parse("QuIt"), Command::Quit);
        assert_eq!(parse("BDAT 10"), Command::Unknown);
    }

    #[test]
    fn mail_from_forms() {
        assert_eq!(
            mail("MAIL FROM:<a@example.com>").0,
            Ok("a@example.com".into())
        );
        assert_eq!(
            mail("mail from:<a@example.com>").0,
            Ok("a@example.com".into())
        );
        assert_eq!(
            mail("MAIL FROM: <a@example.com>").0,
            Ok("a@example.com".into()),
            "one space is accepted"
        );
        assert_eq!(mail("MAIL FROM:<>").0, Ok(String::new()));
        assert_eq!(mail("MAIL FROM:a@example.com").0, Err(PathError::Syntax));
        assert_eq!(mail("MAIL TO:<a@example.com>").0, Err(PathError::Syntax));
        assert_eq!(
            mail("MAIL FROM:<a@example.com>SIZE=1").0,
            Err(PathError::Syntax),
            "params need a space"
        );
    }

    #[test]
    fn params_are_parsed() {
        let (p, params) = mail("MAIL FROM:<a@example.com> SIZE=123 BODY=8BITMIME smtputf8");
        assert_eq!(p, Ok("a@example.com".into()));
        assert_eq!(
            params,
            Ok(vec![
                ("SIZE".into(), Some("123".into())),
                ("BODY".into(), Some("8BITMIME".into())),
                ("SMTPUTF8".into(), None)
            ])
        );
    }

    #[test]
    fn address_validation() {
        assert_eq!(
            mail("RCPT TO:<\"john smith\"@example.com>").0,
            Ok("\"john smith\"@example.com".into()),
            "quoted space ok"
        );
        assert_eq!(
            mail("RCPT TO:<\"a>b\"@example.com>").0,
            Ok("\"a>b\"@example.com".into())
        );
        assert_eq!(
            mail("RCPT TO:<john smith@example.com>").0,
            Err(PathError::InvalidAddress)
        );
        assert_eq!(
            mail("RCPT TO:<>").0,
            Err(PathError::InvalidAddress),
            "null path only for MAIL"
        );
        assert_eq!(mail("RCPT TO:<noat>").0, Err(PathError::InvalidAddress));
        assert_eq!(mail("RCPT TO:<Postmaster>").0, Ok("Postmaster".into()));
        assert_eq!(
            mail("RCPT TO:<@relay.example:user@example.com>").0,
            Ok("user@example.com".into())
        );
        assert_eq!(
            mail("RCPT TO:<کاربر@مثال.ایران>").0,
            Ok("کاربر@مثال.ایران".into()),
            "SMTPUTF8"
        );
        assert_eq!(
            mail("RCPT TO:<a\u{0}b@example.com>").0,
            Err(PathError::InvalidAddress)
        );
        let long = format!("RCPT TO:<{}@example.com>", "a".repeat(1100));
        assert_eq!(mail(&long).0, Err(PathError::TooLong));
    }

    #[test]
    fn auth_param_xtext() {
        assert!(valid_auth_param("<>"));
        assert!(valid_auth_param("user+2Bplus@example.com"));
        assert!(!valid_auth_param("bad=value"));
        assert!(!valid_auth_param("bad+zz"));
        assert!(!valid_auth_param(""));
    }
}
