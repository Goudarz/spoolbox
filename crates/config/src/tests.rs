use std::io::Write;
use std::time::Duration;

use super::*;

fn cli(args: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
    args.iter()
        .map(|(k, v)| (k.to_string(), v.map(str::to_string)))
        .collect()
}

fn env(vars: &[(&str, &str)]) -> Vec<(String, String)> {
    vars.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn load_ok(src: Sources) -> Loaded {
    load(&src).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn defaults_are_safe_and_match_spec() {
    let s = load_ok(Sources::default()).settings;
    assert_eq!(s.http.listen.to_string(), "127.0.0.1:8025");
    assert_eq!(s.smtp.listen.to_string(), "127.0.0.1:1025");
    assert_eq!(s.storage.max_messages, 500);
    assert_eq!(s.smtp.max_message_size, 50 * MIB);
    assert_eq!(s.storage.min_free_disk, 1024 * MIB);
    assert_eq!(s.storage.database, None);
}

#[test]
fn precedence_file_then_mp_env_then_native_env_then_cli() {
    let mut f = tempfile::NamedTempFile::new().expect("tempfile");
    writeln!(
        f,
        "label = \"file\"\n[storage]\nmax_messages = 1\n[http]\nwebroot = \"from-file\""
    )
    .expect("write");
    let path = f.path().to_string_lossy().to_string();

    let s = load_ok(Sources {
        cli: cli(&[("config", Some(&path)), ("max", Some("4"))]),
        env: env(&[
            ("MP_MAX_MESSAGES", "2"),
            ("SPOOLBOX_MAX", "3"),
            ("MP_LABEL", "mp"),
            ("MP_WEBROOT", "mail"),
        ]),
    })
    .settings;
    assert_eq!(s.storage.max_messages, 4, "CLI wins");
    assert_eq!(s.label, "mp", "MP env overrides file");
    assert_eq!(s.http.webroot, "/mail/");
}

#[test]
fn config_file_from_env_and_unknown_keys_rejected() {
    let mut f = tempfile::NamedTempFile::new().expect("tempfile");
    writeln!(f, "[storage]\nmax_mesages = 1").expect("write");
    let err = load(&Sources {
        cli: vec![],
        env: env(&[("SPOOLBOX_CONFIG", &f.path().to_string_lossy())]),
    })
    .expect_err("typo must fail");
    assert!(err.to_string().contains("max_mesages"), "{err}");
}

#[test]
fn compat_bool_env_only_true_values_enable() {
    let s = load_ok(Sources {
        cli: vec![],
        env: env(&[("MP_VERBOSE", "yes")]),
    })
    .settings;
    assert_eq!(s.log.level, LogLevel::Debug);
    let s = load_ok(Sources {
        cli: vec![],
        env: env(&[("MP_VERBOSE", "on")]),
    })
    .settings;
    assert_eq!(
        s.log.level,
        LogLevel::Info,
        "'on' is false for MP_* variables"
    );
}

#[test]
fn empty_env_values_are_ignored() {
    let s = load_ok(Sources {
        cli: vec![],
        env: env(&[("MP_MAX_MESSAGES", ""), ("MP_UI_AUTH", "")]),
    })
    .settings;
    assert_eq!(s.storage.max_messages, 500);
}

#[test]
fn pop3_is_off_by_default_and_needs_users() {
    assert_eq!(load_ok(Sources::default()).settings.pop3, None);

    let s = load_ok(Sources {
        cli: cli(&[("pop3", Some("127.0.0.1:1110"))]),
        env: env(&[("MP_POP3_AUTH", "dev:secret")]),
    })
    .settings
    .pop3
    .expect("pop3");
    assert_eq!(s.listen.port, 1110);
    assert!(s.auth.verify("dev", "secret"));
    assert_eq!(s.tls, None);

    let refused = |cli_args: Vec<(String, Option<String>)>, vars: &[(&str, &str)]| {
        load(&Sources {
            cli: cli_args,
            env: env(vars),
        })
        .expect_err("must refuse")
        .to_string()
    };
    let auth = [("MP_POP3_AUTH", "dev:secret")];
    let err = refused(cli(&[("pop3", Some("127.0.0.1:1110"))]), &[]);
    assert!(err.contains("needs users"), "{err}");
    // Options without the listener would otherwise be silently ignored.
    let err = refused(vec![], &auth);
    assert!(
        err.contains("--pop3") && !err.contains("dev:secret"),
        "{err}"
    );
    let err = refused(cli(&[("pop3-tls-cert", Some("auto"))]), &[]);
    assert!(err.contains("--pop3"), "{err}");
    let err = refused(cli(&[("pop3", Some("0.0.0.0:1110"))]), &auth);
    assert!(err.contains("unencrypted"), "{err}");
    let err = refused(
        cli(&[
            ("pop3", Some("127.0.0.1:1110")),
            ("smtp", Some("127.0.0.1:1110")),
        ]),
        &auth,
    );
    assert!(err.contains("already used"), "{err}");
    let err = refused(
        cli(&[
            ("pop3", Some("127.0.0.1:1110")),
            ("pop3-tls-cert", Some("cert.pem")),
        ]),
        &auth,
    );
    assert!(err.contains("--pop3-tls-key"), "{err}");

    // Allowed on the network once passwords are protected.
    let s = load_ok(Sources {
        cli: cli(&[
            ("pop3", Some("0.0.0.0:1110")),
            ("pop3-tls-cert", Some("auto")),
        ]),
        env: env(&auth),
    })
    .settings;
    assert_eq!(s.pop3.expect("pop3").tls, Some(TlsSource::Auto));
    assert!(
        !format!(
            "{:?}",
            load_ok(Sources {
                cli: cli(&[("pop3", Some("127.0.0.1:1110"))]),
                env: env(&auth),
            })
            .settings
        )
        .contains("secret"),
        "the password is never printed"
    );
}

#[test]
fn harmless_planned_options_warn() {
    let l = load_ok(Sources {
        cli: cli(&[("enable-prometheus", Some("true"))]),
        env: env(&[]),
    });
    let text: Vec<&str> = l.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert!(
        text.iter()
            .any(|m| m.contains("--enable-prometheus") && m.contains("P7")),
        "{text:?}"
    );
}

#[test]
fn deprecated_aliases_map_to_new_names() {
    let l = load_ok(Sources {
        cli: vec![],
        env: env(&[("MP_DATA_FILE", "/tmp/a.db")]),
    });
    assert_eq!(
        l.settings.storage.database.as_deref(),
        Some(std::path::Path::new("/tmp/a.db"))
    );
    assert!(
        l.diagnostics
            .iter()
            .any(|d| d.message.contains("deprecated"))
    );
}

#[test]
fn removed_options_fail() {
    let err = load(&Sources {
        cli: cli(&[("tenant-id", Some("a"))]),
        env: vec![],
    })
    .expect_err("removed");
    assert!(matches!(err, ConfigError::Removed { .. }));
    let err = load(&Sources {
        cli: cli(&[("database", Some("http://rqlite:4001"))]),
        env: vec![],
    })
    .expect_err("rqlite");
    assert!(err.to_string().contains("rqlite"), "{err}");
}

#[test]
fn no_op_flags_are_reported_not_rejected() {
    let l = load_ok(Sources {
        cli: cli(&[("disable-version-check", None), ("smtp-disable-rdns", None)]),
        env: vec![],
    });
    assert_eq!(
        l.diagnostics
            .iter()
            .filter(|d| d.level == DiagnosticLevel::Info)
            .count(),
        2
    );
}

#[test]
fn non_loopback_requires_explicit_opt_in() {
    for addr in ["0.0.0.0:8025", "[::]:8025", ":8025", "192.168.1.5:8025"] {
        let err = load(&Sources {
            cli: cli(&[("listen", Some(addr))]),
            env: vec![],
        })
        .expect_err(addr);
        assert!(
            matches!(err, ConfigError::UnsafeNetwork { what: "HTTP", .. }),
            "{addr}: {err}"
        );
    }
    let s = load_ok(Sources {
        cli: vec![],
        env: env(&[
            ("MP_SMTP_BIND_ADDR", "0.0.0.0:1025"),
            ("SPOOLBOX_ALLOW_UNAUTHENTICATED_NETWORK", "true"),
        ]),
    })
    .settings;
    assert!(s.smtp.listen.is_unspecified());
}

#[test]
fn quiet_and_verbose_conflict() {
    let err = load(&Sources {
        cli: cli(&[("quiet", None)]),
        env: env(&[("MP_VERBOSE", "1")]),
    })
    .expect_err("conflict");
    assert!(matches!(err, ConfigError::Conflict(_)));
}

#[test]
fn same_port_conflict() {
    let err = load(&Sources {
        cli: cli(&[
            ("listen", Some("127.0.0.1:2525")),
            ("smtp", Some("127.0.0.1:2525")),
        ]),
        env: vec![],
    })
    .expect_err("conflict");
    assert!(err.to_string().contains("cannot both listen"));
}

#[test]
fn invalid_values_name_their_origin() {
    let err = load(&Sources {
        cli: vec![],
        env: env(&[("MP_MAX_MESSAGES", "lots")]),
    })
    .expect_err("invalid");
    assert!(err.to_string().contains("MP_MAX_MESSAGES"), "{err}");
    let err = load(&Sources {
        cli: cli(&[("max-age", Some("2w"))]),
        env: vec![],
    })
    .expect_err("invalid");
    assert!(err.to_string().contains("--max-age"), "{err}");
}

#[test]
fn unknown_env_vars_get_suggestions() {
    let l = load_ok(Sources {
        cli: vec![],
        env: env(&[
            ("SPOOLBOX_LISTN", "x"),
            ("MP_MAX_MESAGES", "1"),
            ("PATH", "/bin"),
        ]),
    });
    let text: Vec<&str> = l.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert!(
        text.iter()
            .any(|m| m.contains("SPOOLBOX_LISTN") && m.contains("SPOOLBOX_LISTEN")),
        "{text:?}"
    );
    assert!(
        text.iter()
            .any(|m| m.contains("MP_MAX_MESAGES") && m.contains("MP_MAX_MESSAGES")),
        "{text:?}"
    );
    assert!(!text.iter().any(|m| m.contains("PATH")));
}

#[test]
fn max_age_and_sizes() {
    let s = load_ok(Sources {
        cli: cli(&[
            ("max-age", Some("3d")),
            ("max-message-size", Some("0")),
            ("min-free-disk", Some("0")),
        ]),
        env: vec![],
    })
    .settings;
    assert_eq!(s.storage.max_age, Some(Duration::from_secs(3 * 86_400)));
    assert_eq!(s.smtp.max_message_size, 0);
    assert_eq!(s.storage.min_free_disk, 0);
}

#[test]
fn smtp_tls_options() {
    let s = load_ok(Sources {
        cli: cli(&[
            ("smtp-tls-cert", Some("auto")),
            ("smtp-require-starttls", None),
        ]),
        env: vec![],
    })
    .settings;
    assert_eq!(s.smtp.tls, Some(TlsSource::Auto));
    assert!(s.smtp.require_starttls);
    let s = load_ok(Sources {
        cli: vec![],
        env: env(&[
            ("MP_SMTP_TLS_CERT", "c.pem"),
            ("MP_SMTP_TLS_KEY", "k.pem"),
            ("MP_SMTP_REQUIRE_TLS", "1"),
        ]),
    })
    .settings;
    assert_eq!(
        s.smtp.tls,
        Some(TlsSource::Files {
            cert: "c.pem".into(),
            key: "k.pem".into()
        })
    );
    assert!(s.smtp.implicit_tls);
    for bad in [
        &[("smtp-tls-cert", Some("c.pem"))][..],
        &[("smtp-tls-key", Some("k.pem"))],
        &[("smtp-require-tls", None)],
    ] {
        assert!(
            matches!(
                load(&Sources {
                    cli: cli(bad),
                    env: vec![]
                }),
                Err(ConfigError::Conflict(_))
            ),
            "{bad:?}"
        );
    }
}

#[test]
fn smtp_policy_options() {
    let s = load_ok(Sources {
        cli: cli(&[
            ("smtp-hide-extensions", Some("pipelining, SMTPUTF8")),
            ("smtp-reject-domains", Some("Spam.test")),
            ("smtp-max-connections", Some("5")),
        ]),
        env: env(&[
            ("MP_SMTP_ALLOWED_RECIPIENTS", r"@example\.com$"),
            ("MP_SMTP_IGNORE_REJECTED_RECIPIENTS", "true"),
            ("MP_SMTP_AUTH_ACCEPT_ANY", "1"),
            ("MP_SMTP_AUTH_ALLOW_INSECURE", "1"),
        ]),
    })
    .settings;
    assert_eq!(s.smtp.hide_extensions, ["PIPELINING", "SMTPUTF8"]);
    assert_eq!(s.smtp.reject_domains, ["spam.test"]);
    assert_eq!(s.smtp.max_connections, 5);
    assert_eq!(
        s.smtp.allowed_recipients.as_deref(),
        Some(r"@example\.com$")
    );
    assert!(s.smtp.ignore_rejected_recipients && s.smtp.auth_accept_any);
    let err = load(&Sources {
        cli: cli(&[("smtp-allowed-recipients", Some("(unclosed"))]),
        env: vec![],
    })
    .expect_err("regex");
    assert!(err.to_string().contains("regular expression"), "{err}");
    let err = load(&Sources {
        cli: cli(&[("smtp-hide-extensions", Some("DSN"))]),
        env: vec![],
    })
    .expect_err("unknown ext");
    assert!(err.to_string().contains("DSN"), "{err}");
}

#[test]
fn storage_and_tag_options() {
    let s = load_ok(Sources {
        cli: cli(&[("compression", Some("3")), ("disable-wal", None)]),
        env: env(&[
            ("MP_TAGS_DISABLE", "plus-addresses"),
            ("MP_TAGS_USERNAME", "true"),
            ("MP_IGNORE_DUPLICATE_IDS", "1"),
            ("MP_DISABLE_AUTO_VACUUM", "yes"),
        ]),
    })
    .settings;
    assert_eq!(s.storage.compression, 3);
    assert!(!s.storage.wal && !s.storage.auto_vacuum && s.storage.ignore_duplicate_ids);
    assert!(!s.tags.plus_addresses && s.tags.x_tags && s.tags.username);
    assert!(
        load(&Sources {
            cli: cli(&[("compression", Some("9"))]),
            env: vec![]
        })
        .is_err()
    );
}

#[test]
fn auth_credentials_from_env_and_files() {
    let mut f = tempfile::NamedTempFile::new().expect("tempfile");
    writeln!(
        f,
        "# users
file-user:secret"
    )
    .expect("write");
    let path = f.path().to_str().expect("utf-8 path");
    let l = load_ok(Sources {
        cli: cli(&[
            ("smtp-auth-file", Some(path)),
            ("smtp-tls-cert", Some("auto")),
        ]),
        env: env(&[
            ("MP_UI_AUTH", "a:1 b:2"),
            ("MP_SEND_API_AUTH", "sender:pw"),
            ("SPOOLBOX_UI_AUTH_FILE", path),
        ]),
    });
    let s = &l.settings;
    // The file wins over inline entries.
    let ui = s.http.auth.as_ref().expect("ui auth");
    assert!(ui.verify("file-user", "secret") && !ui.verify("a", "1"));
    assert!(
        l.diagnostics
            .iter()
            .any(|d| d.message.contains("MP_UI_AUTH is ignored"))
    );
    assert!(
        s.http
            .send_auth
            .as_ref()
            .expect("send")
            .verify("sender", "pw")
    );
    assert!(
        s.smtp
            .auth
            .as_ref()
            .expect("smtp")
            .verify("file-user", "secret")
    );
    assert!(
        s.smtp.require_starttls,
        "credentials force STARTTLS (RFC 4954)"
    );
    assert!(!format!("{s:?}").contains("secret"));
}

#[test]
fn smtp_auth_rules() {
    let refuse = |cli_args: &[(&str, Option<&str>)], vars: &[(&str, &str)], needle: &str| {
        let err = load(&Sources {
            cli: cli(cli_args),
            env: env(vars),
        })
        .expect_err(needle);
        assert!(err.to_string().contains(needle), "{err}");
        assert!(!err.to_string().contains("hunter2"), "secret echoed: {err}");
    };
    refuse(
        &[],
        &[("MP_SMTP_AUTH", "user:hunter2")],
        "requires STARTTLS or TLS",
    );
    refuse(
        &[("smtp-auth-accept-any", None)],
        &[],
        "requires STARTTLS or TLS",
    );
    refuse(
        &[
            ("smtp-auth-accept-any", None),
            ("smtp-auth-allow-insecure", None),
        ],
        &[("MP_SMTP_AUTH", "user:hunter2")],
        "both credentials and --smtp-auth-accept-any",
    );
    refuse(
        &[
            ("smtp-tls-cert", Some("auto")),
            ("smtp-require-tls", None),
            ("smtp-auth-allow-insecure", None),
        ],
        &[],
        "--smtp-auth-allow-insecure",
    );
    refuse(
        &[
            ("smtp-tls-cert", Some("auto")),
            ("smtp-require-tls", None),
            ("smtp-require-starttls", None),
        ],
        &[],
        "cannot be used together",
    );
    refuse(
        &[("send-api-auth-accept-any", None)],
        &[("MP_SEND_API_AUTH", "u:hunter2")],
        "--send-api-auth-accept-any",
    );
    refuse(
        &[("ui-auth-file", Some("does-not-exist.htpasswd"))],
        &[],
        "cannot read password file",
    );
    refuse(&[], &[("MP_UI_AUTH", "no-colon")], "expected user:password");

    let s = load_ok(Sources {
        cli: cli(&[("smtp-auth-allow-insecure", None)]),
        env: env(&[("MP_SMTP_AUTH", "user:hunter2")]),
    })
    .settings;
    assert!(!s.smtp.require_starttls && s.smtp.auth.is_some());
    let s = load_ok(Sources {
        cli: cli(&[("smtp-tls-cert", Some("auto")), ("smtp-require-tls", None)]),
        env: env(&[("MP_SMTP_AUTH", "user:hunter2")]),
    })
    .settings;
    assert!(s.smtp.implicit_tls && !s.smtp.require_starttls);
}

#[test]
fn auth_allows_non_loopback_listeners() {
    let s = load_ok(Sources {
        cli: cli(&[
            ("listen", Some("0.0.0.0:8025")),
            ("smtp", Some("127.0.0.1:1025")),
        ]),
        env: env(&[("MP_UI_AUTH", "user:pass")]),
    })
    .settings;
    assert!(!s.http.listen.is_loopback());
    let err = load(&Sources {
        cli: cli(&[("smtp", Some("0.0.0.0:1025"))]),
        env: env(&[("MP_UI_AUTH", "user:pass")]),
    })
    .expect_err("SMTP still unauthenticated");
    assert!(
        matches!(err, ConfigError::UnsafeNetwork { what: "SMTP", .. }),
        "{err}"
    );
}

#[test]
fn cors_and_compression() {
    let l = load_ok(Sources {
        cli: cli(&[
            (
                "api-cors",
                Some("https://App.test:3000/path, other.test,*.bad.test"),
            ),
            ("disable-http-compression", None),
        ]),
        env: vec![],
    });
    assert_eq!(
        l.settings.http.cors_origins,
        ["app.test:3000", "other.test"]
    );
    assert!(!l.settings.http.compression);
    assert!(
        l.diagnostics
            .iter()
            .any(|d| d.message.contains("*.bad.test"))
    );
    let s = load_ok(Sources {
        cli: vec![],
        env: env(&[("MP_API_CORS", "a.test *")]),
    })
    .settings;
    assert_eq!(s.http.cors_origins, ["*"]);
}

#[test]
fn chaos_options() {
    let s = load_ok(Sources::default()).settings;
    assert!(!s.chaos.enabled);
    let s = load_ok(Sources {
        cli: vec![],
        env: env(&[("MP_CHAOS_TRIGGERS", "Sender:550:10,Authentication:535:5")]),
    })
    .settings;
    assert!(s.chaos.enabled, "triggers imply --enable-chaos");
    assert_eq!(s.chaos.triggers.sender.error_code, 550);
    assert_eq!(s.chaos.triggers.authentication.probability, 5);
    let s = load_ok(Sources {
        cli: cli(&[("enable-chaos", None)]),
        env: vec![],
    })
    .settings;
    assert!(s.chaos.enabled && s.chaos.triggers.sender.probability == 0);
    let err = load(&Sources {
        cli: cli(&[("chaos-triggers", Some("Sender:200:1"))]),
        env: vec![],
    })
    .expect_err("invalid code");
    assert!(err.to_string().contains("between 400 and 599"), "{err}");
}

#[test]
fn ui_tls_and_ui_flags() {
    let s = load_ok(Sources {
        cli: cli(&[
            ("ui-tls-cert", Some("auto")),
            ("hide-delete-all-button", None),
        ]),
        env: env(&[("MP_BLOCK_REMOTE_CSS_AND_FONTS", "1")]),
    })
    .settings;
    assert_eq!(s.http.tls, Some(TlsSource::Auto));
    assert!(s.http.hide_delete_all_button && s.http.block_remote_css_and_fonts);
    let err = load(&Sources {
        cli: cli(&[("ui-tls-cert", Some("cert.pem"))]),
        env: vec![],
    })
    .expect_err("key missing");
    assert!(err.to_string().contains("--ui-tls-key"), "{err}");
}

#[test]
fn chaos_rules_from_the_config_file() {
    let mut f = tempfile::NamedTempFile::new().expect("tempfile");
    write!(
        f,
        r#"
[chaos]
[[chaos.rules]]
stage = "recipient"
match = "@flaky[.]test$"
code = 452
message = "4.2.2 Mailbox full"
times = 2

[[chaos.rules]]
stage = "connect"
delay_ms = 1500
"#
    )
    .expect("write");
    let s = load_ok(Sources {
        cli: cli(&[("config", f.path().to_str())]),
        env: vec![],
    })
    .settings;
    assert!(s.chaos.enabled, "rules imply chaos");
    assert_eq!(s.chaos.rules.len(), 2);
    assert_eq!(s.chaos.rules[0].times, Some(2));
    assert_eq!(s.chaos.rules[1].delay_ms, 1500);

    let mut bad = tempfile::NamedTempFile::new().expect("tempfile");
    write!(bad, "[[chaos.rules]]\nstage = \"quit\"\ncode = 451\n").expect("write");
    let err = load(&Sources {
        cli: cli(&[("config", bad.path().to_str())]),
        env: vec![],
    })
    .expect_err("bad stage");
    assert!(err.to_string().contains("chaos.rules[0]"), "{err}");
}

#[test]
fn webhook_options() {
    let s = load_ok(Sources {
        cli: cli(&[
            ("webhook-url", Some("https://hooks.example.com/mail")),
            ("webhook-delay", Some("2")),
        ]),
        env: env(&[
            ("MP_WEBHOOK_LIMIT", "5"),
            ("SPOOLBOX_WEBHOOK_SECRET", "s3cret-key"),
        ]),
    })
    .settings;
    assert_eq!(
        s.webhook.url.as_deref(),
        Some("https://hooks.example.com/mail")
    );
    assert_eq!(
        (s.webhook.limit_per_second, s.webhook.delay),
        (5, Duration::from_secs(2))
    );
    assert!(
        !format!("{s:?}").contains("s3cret-key"),
        "secret never printed"
    );
    for bad in ["ftp://x", "http://", "http://a b/"] {
        let err = load(&Sources {
            cli: cli(&[("webhook-url", Some(bad))]),
            env: vec![],
        })
        .expect_err(bad);
        assert!(err.to_string().contains("http://"), "{bad}: {err}");
    }
}

fn relay_env(extra: &[(&'static str, &'static str)]) -> Vec<(String, String)> {
    let mut all = vec![("MP_SMTP_RELAY_HOST", "smtp.example.com")];
    all.extend_from_slice(extra);
    env(&all)
}

#[test]
fn relay_is_off_by_default_and_read_from_variables() {
    let s = load_ok(Sources::default()).settings;
    assert_eq!(s.relay, RelaySettings::default());
    assert_eq!(s.forward, ForwardSettings::default());

    let s = load_ok(Sources {
        cli: vec![],
        env: relay_env(&[
            ("MP_SMTP_RELAY_PORT", "587"),
            ("MP_SMTP_RELAY_STARTTLS", "true"),
            ("MP_SMTP_RELAY_USERNAME", "user"),
            ("MP_SMTP_RELAY_PASSWORD", "s3cret"),
            ("MP_SMTP_RELAY_ALLOWED_RECIPIENTS", "@example\\.com$"),
        ]),
    })
    .settings;
    let target = s.relay.target.as_ref().expect("target");
    assert_eq!(
        (target.host.as_str(), target.port),
        ("smtp.example.com", 587)
    );
    assert_eq!(target.security, SmtpSecurity::StartTls);
    assert_eq!(target.limit_per_minute, 60);
    assert_eq!(target.login.as_ref().expect("login").password, "s3cret");
    assert!(!s.relay.all, "releasing does not relay new messages");
    assert!(
        !format!("{s:?}").contains("s3cret"),
        "the password is never printed"
    );
}

#[test]
fn relaying_everything_needs_an_allowlist() {
    let err = load(&Sources {
        cli: cli(&[("smtp-relay-all", None)]),
        env: relay_env(&[]),
    })
    .expect_err("open relay");
    assert!(err.to_string().contains("allowed-recipients"), "{err}");

    let s = load_ok(Sources {
        cli: cli(&[("smtp-relay-all", None)]),
        env: relay_env(&[("MP_SMTP_RELAY_ALLOWED_RECIPIENTS", "@example\\.com$")]),
    })
    .settings;
    assert!(s.relay.all);

    let err = load(&Sources {
        cli: cli(&[("smtp-relay-all", None)]),
        env: vec![],
    })
    .expect_err("no host");
    assert!(err.to_string().contains("host"), "{err}");
}

#[test]
fn unsafe_relay_settings_are_refused() {
    for (extra, expected) in [
        (
            &[
                ("MP_SMTP_RELAY_USERNAME", "u"),
                ("MP_SMTP_RELAY_PASSWORD", "s3cret"),
            ][..],
            "unencrypted",
        ),
        (&[("MP_SMTP_RELAY_USERNAME", "u")][..], "password"),
        (&[("MP_SMTP_RELAY_AUTH", "cram-md5")][..], "CRAM-MD5"),
        (
            &[("MP_SMTP_RELAY_STARTTLS", "1"), ("MP_SMTP_RELAY_TLS", "1")][..],
            "together",
        ),
        (
            &[("MP_SMTP_RELAY_RETURN_PATH", "a@b\r\nX: y")][..],
            "return-path",
        ),
        (
            &[("MP_SMTP_RELAY_BLOCKED_RECIPIENTS", "(")][..],
            "regular expression",
        ),
    ] {
        let err = load(&Sources {
            cli: vec![],
            env: relay_env(extra),
        })
        .expect_err(expected)
        .to_string();
        assert!(err.contains(expected), "{expected}: {err}");
        assert!(!err.contains("s3cret"), "{err}");
    }
    // Settings without a host would otherwise be silently ignored.
    let err = load(&Sources {
        cli: vec![],
        env: env(&[("MP_SMTP_RELAY_PASSWORD", "s3cret")]),
    })
    .expect_err("no host")
    .to_string();
    assert!(err.contains("host") && !err.contains("s3cret"), "{err}");
    // CRAM-MD5 secrets are refused rather than ignored.
    assert!(
        load(&Sources {
            cli: vec![],
            env: relay_env(&[("MP_SMTP_RELAY_SECRET", "s3cret")]),
        })
        .is_err()
    );
}

#[test]
fn relay_to_this_server_is_refused() {
    let err = load(&Sources {
        cli: cli(&[("smtp", Some("127.0.0.1:2525"))]),
        env: env(&[
            ("MP_SMTP_RELAY_HOST", "localhost"),
            ("MP_SMTP_RELAY_PORT", "2525"),
        ]),
    })
    .expect_err("loop");
    assert!(err.to_string().contains("come back"), "{err}");
}

#[test]
fn relay_file_is_overridden_by_variables() {
    let mut f = tempfile::NamedTempFile::new().expect("tempfile");
    writeln!(
        f,
        "host: from-file.example.com\nport: 2525\nrate-limit: 5\nallowed-recipients: '@file$'"
    )
    .expect("write");
    let path = f.path().to_string_lossy().to_string();
    let s = load_ok(Sources {
        cli: cli(&[("smtp-relay-config", Some(&path))]),
        env: env(&[("MP_SMTP_RELAY_HOST", "from-env.example.com")]),
    })
    .settings;
    let target = s.relay.target.expect("target");
    assert_eq!(
        (target.host.as_str(), target.port),
        ("from-env.example.com", 2525)
    );
    assert_eq!(target.limit_per_minute, 5);
    assert_eq!(s.relay.allowed_recipients.as_deref(), Some("@file$"));
}

#[test]
fn forward_needs_addresses() {
    let s = load_ok(Sources {
        cli: vec![],
        env: env(&[
            ("MP_SMTP_FORWARD_HOST", "smtp.example.com"),
            ("MP_SMTP_FORWARD_TO", "a@example.com, b@example.com"),
        ]),
    })
    .settings;
    assert_eq!(s.forward.to, ["a@example.com", "b@example.com"]);

    for (vars, expected) in [
        (&[("MP_SMTP_FORWARD_HOST", "h")][..], "at least one"),
        (&[("MP_SMTP_FORWARD_TO", "a@b.test")][..], "host"),
        (
            &[
                ("MP_SMTP_FORWARD_HOST", "h"),
                ("MP_SMTP_FORWARD_TO", "A <a@b.test>"),
            ][..],
            "not a valid address",
        ),
    ] {
        let err = load(&Sources {
            cli: vec![],
            env: env(vars),
        })
        .expect_err(expected);
        assert!(err.to_string().contains(expected), "{err}");
    }
}

#[test]
fn native_relay_variables_win_over_compatibility_ones() {
    let s = load_ok(Sources {
        cli: vec![],
        env: env(&[
            ("MP_SMTP_RELAY_HOST", "from-mp.example.com"),
            ("SPOOLBOX_SMTP_RELAY_HOST", "from-native.example.com"),
            ("SPOOLBOX_SMTP_RELAY_STARTTLS", "on"),
            ("SPOOLBOX_SMTP_RELAY_RATE_LIMIT", "7"),
            ("SPOOLBOX_SMTP_FORWARD_HOST", "forward.example.com"),
            ("SPOOLBOX_SMTP_FORWARD_TO", "team@example.com"),
        ]),
    });
    assert!(
        s.diagnostics.iter().all(|d| !d.message.contains("unknown")),
        "{:?}",
        s.diagnostics
    );
    let target = s.settings.relay.target.expect("target");
    assert_eq!(target.host, "from-native.example.com");
    assert_eq!(target.security, SmtpSecurity::StartTls);
    assert_eq!(target.limit_per_minute, 7);
    assert_eq!(s.settings.forward.to, ["team@example.com"]);

    // A mistyped boolean is an error, not a silently disabled TLS.
    let err = load(&Sources {
        cli: vec![],
        env: env(&[
            ("SPOOLBOX_SMTP_RELAY_HOST", "h"),
            ("SPOOLBOX_SMTP_RELAY_TLS", "maybe"),
        ]),
    })
    .expect_err("bad boolean");
    assert!(err.to_string().contains("true or false"), "{err}");
    // No such option: reported rather than ignored.
    let l = load_ok(Sources {
        cli: vec![],
        env: env(&[("SPOOLBOX_SMTP_RELAY_HOTS", "h")]),
    });
    assert!(
        l.diagnostics.iter().any(|d| d.message.contains("HOTS")),
        "{:?}",
        l.diagnostics
    );
}

#[test]
fn relay_webhook_and_network_in_the_config_file() {
    let mut yaml = tempfile::NamedTempFile::new().expect("tempfile");
    writeln!(yaml, "host: smtp.example.com\nallowed-recipients: '@a$'").expect("write");
    let mut toml = tempfile::NamedTempFile::new().expect("tempfile");
    // Literal strings, so Windows paths need no escaping.
    writeln!(
        toml,
        "[relay]\nconfig = '{}'\nall = true\n[forward]\n[webhook]\nurl = 'https://hooks.example.com/'\nlimit_per_second = 3\ndelay_seconds = 4\nsecret = 'k'\n[network]\nallow_internal_http_requests = true\nallow_untrusted_tls = true",
        yaml.path().display()
    )
    .expect("write");
    let path = toml.path().to_string_lossy().to_string();
    let s = load_ok(Sources {
        cli: cli(&[("config", Some(&path))]),
        env: vec![],
    })
    .settings;
    assert!(s.relay.all && s.relay.target.is_some());
    assert_eq!(s.webhook.url.as_deref(), Some("https://hooks.example.com/"));
    assert_eq!(
        (s.webhook.limit_per_second, s.webhook.delay),
        (3, Duration::from_secs(4))
    );
    assert!(s.egress.allow_internal && s.egress.allow_untrusted_tls);
}

#[test]
fn link_check_options() {
    let s = load_ok(Sources::default()).settings.egress;
    assert!(!s.link_check_disabled && s.link_check_rate_limit);
    let s = load_ok(Sources {
        cli: cli(&[("disable-link-check", None)]),
        env: env(&[("MP_DISABLE_LINK_CHECK_RATE_LIMIT", "true")]),
    })
    .settings
    .egress;
    assert!(s.link_check_disabled && !s.link_check_rate_limit);
}
