use clap::parser::ValueSource;
use clap::{Arg, ArgAction, ArgMatches, Command};
use spoolbox_config::registry::{FLAGS, Status};

pub fn command() -> Command {
    let server_args = FLAGS.iter().map(|f| {
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
    });

    Command::new("spoolbox")
        .version(spoolbox_core::VERSION)
        .about("Email and SMTP testing tool for developers. Without a subcommand, runs the server.")
        .args_conflicts_with_subcommands(true)
        .args(server_args)
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
