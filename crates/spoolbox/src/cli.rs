use clap::parser::ValueSource;
use clap::{Arg, ArgAction, ArgMatches, Command};
use spoolbox_config::registry::{FLAGS, Status};

/// Options of the commands that talk to a running server.
fn client_args(cmd: Command) -> Command {
    cmd.arg(
        Arg::new("listen")
            .long("listen")
            .short('l')
            .value_name("ADDR")
            .help("HTTP address of the server (default: from config/env)"),
    )
    .arg(
        Arg::new("webroot")
            .long("webroot")
            .value_name("PATH")
            .help("Webroot of the server (default: from config/env)"),
    )
    .arg(
        Arg::new("https")
            .long("https")
            .action(ArgAction::SetTrue)
            .help("The server uses HTTPS (implied by --ui-tls-cert in the config)"),
    )
    .arg(
        Arg::new("auth")
            .long("auth")
            .value_name("USER:PASSWORD")
            .help("Credentials for a server with --ui-auth-file (or SPOOLBOX_AUTH)"),
    )
}

pub fn command() -> Command {
    let server_args = || {
        FLAGS.iter().map(|f| {
            let mut arg = Arg::new(f.long).long(f.long);
            if let Some(s) = f.short {
                arg = arg.short(s);
            }
            arg = if f.takes_value {
                arg.action(ArgAction::Set).value_name("VALUE")
            } else {
                arg.action(ArgAction::SetTrue)
            };
            // Compatibility options that are not implemented yet are accepted (and reported), but not advertised.
            if matches!(f.status, Status::Active(_)) {
                arg.help(f.help)
            } else {
                arg.hide(true)
            }
        })
    };

    Command::new("spoolbox")
        .version(spoolbox_core::VERSION)
        .about("Email and SMTP testing tool for developers. Without a subcommand, runs the server.")
        .args_conflicts_with_subcommands(true)
        .args(server_args())
        .subcommand(
            Command::new("doctor")
                .about("Check configuration, ports, TLS and storage, and explain problems (takes the server's flags)")
                .args(server_args()),
        )
        .subcommand(
            Command::new("readyz")
                .about("Check whether a running Spoolbox is ready (exit code 0 = ready)")
                .arg(
                    Arg::new("listen")
                        .long("listen")
                        .short('l')
                        .value_name("ADDR")
                        .help("HTTP address of the server (default: from config/env)"),
                )
                .arg(
                    Arg::new("webroot")
                        .long("webroot")
                        .value_name("PATH")
                        .help("Webroot of the server (default: from config/env)"),
                )
                .arg(
                    Arg::new("wait")
                        .long("wait")
                        .action(ArgAction::SetTrue)
                        .help("Wait until ready instead of checking once"),
                )
                .arg(
                    Arg::new("timeout")
                        .long("timeout")
                        .value_name("DURATION")
                        .default_value("30s")
                        .help("Maximum wait with --wait, e.g. 500ms, 30s, 2m"),
                )
                .arg(
                    Arg::new("https")
                        .long("https")
                        .action(ArgAction::SetTrue)
                        .hide(true),
                ),
        )
        .subcommand(Command::new("version").about("Print version information"))
        .subcommand(client_args(
            Command::new("wait")
                .about("Wait until a message matching a search arrives; prints its ID (exit code 1 on timeout)")
                .arg(Arg::new("query").required(true).value_name("QUERY").help("Search, e.g. 'to:alice@example.com subject:verify'"))
                .arg(Arg::new("timeout").long("timeout").short('t').value_name("DURATION").default_value("30s").help("How long to wait, e.g. 10s, 2m"))
                .arg(Arg::new("since").long("since").value_name("SEQ").value_parser(clap::value_parser!(i64)).help("Only mail after this event cursor (GET api/v2/events/cursor)"))
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue).help("Print the message summary as JSON")),
        ))
        .subcommand(client_args(
            Command::new("export")
                .about("Save messages from a running server as .eml files or one mbox file")
                .arg(Arg::new("out").required(true).value_name("PATH").help("Directory for .eml files, or file for --format mbox"))
                .arg(Arg::new("query").long("query").short('q').value_name("QUERY").help("Only messages matching this search"))
                .arg(Arg::new("format").long("format").value_parser(["eml", "mbox"]).default_value("eml")),
        ))
        .subcommand(client_args(
            Command::new("mcp")
                .about("Model Context Protocol server on stdio, for AI assistants (connects to a running server's /mcp)"),
        ))
        .subcommand(client_args(
            Command::new("import")
                .about("Load .eml files, directories of them, or mbox files into a running server")
                .arg(Arg::new("paths").required(true).num_args(1..).value_name("PATH")),
        ))
        .subcommand(
            Command::new("reindex")
                .about("Rebuild the MIME index and full-text search of every message in a database")
                .arg(Arg::new("database").required(true).value_name("DATABASE")),
        )
        .subcommand(
            Command::new("send-sample")
                .about("Send sample messages (Persian/RTL, attachments, inline images, OTP...) to a running server")
                .arg(Arg::new("smtp").long("smtp").short('s').value_name("ADDR").help("SMTP address (default: from config/env)"))
                .arg(Arg::new("to").long("to").value_name("ADDRESS").help("Recipient (default: developer@example.com)"))
                .arg(Arg::new("sample").long("sample").value_name("NAME").help("One sample by name, or `all` (default)")),
        )
}

/// Flags given on the command line (not defaults), by long name; booleans have no value.
pub fn given_flags(m: &ArgMatches) -> Vec<(String, Option<String>)> {
    FLAGS
        .iter()
        .filter(|f| m.value_source(f.long) == Some(ValueSource::CommandLine))
        .filter_map(|f| {
            if f.takes_value {
                m.get_one::<String>(f.long)
                    .map(|v| (f.long.to_string(), Some(v.clone())))
            } else {
                m.get_flag(f.long).then(|| (f.long.to_string(), None))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_is_valid() {
        command().debug_assert();
    }

    #[test]
    fn accepts_compatibility_flags() {
        let m = command()
            .try_get_matches_from([
                "spoolbox",
                "-m",
                "10",
                "--smtp",
                "127.0.0.1:2525",
                "--webhook-url",
                "http://x",
                "-q",
            ])
            .expect("parse");
        let given = given_flags(&m);
        assert!(given.contains(&("max".into(), Some("10".into()))));
        assert!(given.contains(&("smtp".into(), Some("127.0.0.1:2525".into()))));
        assert!(given.contains(&("webhook-url".into(), Some("http://x".into()))));
        assert!(given.contains(&("quiet".into(), None)));
        assert_eq!(given.len(), 4);
    }

    #[test]
    fn unknown_flag_gets_suggestion() {
        let err = command()
            .try_get_matches_from(["spoolbox", "--lisen", "x"])
            .expect_err("unknown");
        assert!(err.to_string().contains("--listen"), "{err}");
    }
}
