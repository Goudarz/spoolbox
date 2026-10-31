//! Chaos testing: SMTP errors returned at random for MAIL, RCPT and AUTH
//! (`--enable-chaos`, `--chaos-triggers`, `GET/PUT /api/v1/chaos`).

use std::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trigger {
    /// SMTP reply code, 400–599.
    pub error_code: u16,
    /// Percent, 0–100.
    pub probability: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Triggers {
    pub sender: Trigger,
    pub recipient: Trigger,
    pub authentication: Trigger,
}

impl Default for Triggers {
    fn default() -> Self {
        let off = |error_code| Trigger {
            error_code,
            probability: 0,
        };
        Self {
            sender: off(451),
            recipient: off(451),
            authentication: off(535),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Sender,
    Recipient,
    Authentication,
}

impl Triggers {
    /// Sets one trigger by key (`sender`, `recipient(s)`, `auth(entication)`).
    pub fn set(&mut self, key: &str, error_code: u16, probability: u8) -> Result<(), String> {
        if !(400..=599).contains(&error_code) {
            return Err("error code must be between 400 and 599".into());
        }
        if probability > 100 {
            return Err("probability must be between 0 and 100".into());
        }
        let t = Trigger {
            error_code,
            probability,
        };
        match key.to_ascii_lowercase().as_str() {
            "sender" => self.sender = t,
            "recipient" | "recipients" => self.recipient = t,
            "auth" | "authentication" => self.authentication = t,
            other => return Err(format!("unknown key {other}")),
        }
        Ok(())
    }

    /// Parses `--chaos-triggers`: `Key:code:probability` entries separated by commas.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut out = Self::default();
        for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let invalid = || format!("invalid argument: {entry}");
            let mut it = entry.split(':');
            let (Some(key), Some(code), Some(prob), None) =
                (it.next(), it.next(), it.next(), it.next())
            else {
                return Err(invalid());
            };
            let code: u16 = code.parse().map_err(|_| invalid())?;
            let prob: u8 = prob.parse().map_err(|_| invalid())?;
            out.set(key, code, prob)?;
        }
        Ok(out)
    }

    fn get(&self, stage: Stage) -> Trigger {
        match stage {
            Stage::Sender => self.sender,
            Stage::Recipient => self.recipient,
            Stage::Authentication => self.authentication,
        }
    }
}

/// Where a rule applies in the SMTP conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleStage {
    /// Before the greeting; `match` is the client IP.
    Connect,
    /// `MAIL FROM`; `match` is the sender.
    Sender,
    /// `RCPT TO`; `match` is the recipient.
    Recipient,
    /// The `DATA` command; `match` is the sender.
    Data,
    /// After the body was received (the message is not stored); `match` is the sender.
    Message,
    /// `AUTH`.
    Auth,
}

impl RuleStage {
    pub fn parse(s: &str) -> Result<Self, String> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "connect" => Self::Connect,
            "sender" | "mail" => Self::Sender,
            "recipient" | "rcpt" => Self::Recipient,
            "data" => Self::Data,
            "message" => Self::Message,
            "auth" | "authentication" => Self::Auth,
            other => {
                return Err(format!(
                    "unknown stage {other:?} (expected connect, sender, recipient, data, message, auth)"
                ));
            }
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Sender => "sender",
            Self::Recipient => "recipient",
            Self::Data => "data",
            Self::Message => "message",
            Self::Auth => "auth",
        }
    }
}

/// A deterministic chaos rule: rules are checked in order and the first one that applies fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub stage: RuleStage,
    /// Regular expression the stage's subject must match (see [`RuleStage`]).
    pub pattern: Option<String>,
    /// Reply with this code instead of the normal reply.
    pub code: Option<u16>,
    /// Text after the code (default: `Chaos rule`).
    pub message: Option<String>,
    /// Wait this long before replying.
    pub delay_ms: u64,
    /// Close the connection without replying.
    pub disconnect: bool,
    /// Fire at most this many times; `None` = always.
    pub times: Option<u32>,
}

impl Rule {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(code) = self.code
            && !(400..=599).contains(&code)
        {
            return Err("code must be between 400 and 599".into());
        }
        if self.code.is_none() && self.delay_ms == 0 && !self.disconnect {
            return Err("a rule needs a code, delay_ms or disconnect".into());
        }
        if self.delay_ms > 600_000 {
            return Err("delay_ms must be at most 600000".into());
        }
        if let Some(m) = &self.message
            && m.contains(['\r', '\n'])
        {
            return Err("message must be a single line".into());
        }
        if let Some(p) = &self.pattern {
            if p.len() > 500 {
                return Err("match must be at most 500 characters".into());
            }
            regex::Regex::new(p).map_err(|e| format!("invalid match: {e}"))?;
        }
        Ok(())
    }
}

/// What a chaos rule or trigger asks the SMTP server to do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Effect {
    /// Complete reply line (`452 ...`) replacing the normal reply.
    pub reply: Option<String>,
    pub delay: std::time::Duration,
    pub disconnect: bool,
}

#[derive(Debug)]
struct RuleState {
    rule: Rule,
    regex: Option<regex::Regex>,
    fired: u32,
}

/// Shared between the SMTP sessions and the HTTP API.
#[derive(Debug, Default)]
pub struct Chaos {
    triggers: RwLock<Triggers>,
    rules: std::sync::Mutex<Vec<RuleState>>,
}

impl Chaos {
    pub fn new(triggers: Triggers) -> Self {
        Self {
            triggers: RwLock::new(triggers),
            rules: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Replaces the rules (all are validated first).
    pub fn set_rules(&self, rules: Vec<Rule>) -> Result<(), String> {
        let mut states = Vec::with_capacity(rules.len());
        for (i, rule) in rules.into_iter().enumerate() {
            rule.validate()
                .map_err(|e| format!("rule {}: {e}", i + 1))?;
            let regex = match &rule.pattern {
                Some(p) => Some(regex::Regex::new(p).map_err(|e| e.to_string())?),
                None => None,
            };
            states.push(RuleState {
                rule,
                regex,
                fired: 0,
            });
        }
        *self.rules.lock().unwrap_or_else(|e| e.into_inner()) = states;
        Ok(())
    }

    /// Rules with how often each has fired.
    pub fn rules(&self) -> Vec<(Rule, u32)> {
        self.rules
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|s| (s.rule.clone(), s.fired))
            .collect()
    }

    /// The first rule for this stage that matches `subject` and has not used up its `times`;
    /// otherwise the random triggers for MAIL, RCPT and AUTH.
    pub fn decide(&self, stage: RuleStage, subject: &str) -> Option<Effect> {
        {
            let mut rules = self.rules.lock().unwrap_or_else(|e| e.into_inner());
            let hit = rules.iter_mut().find(|s| {
                s.rule.stage == stage
                    && s.rule.times.is_none_or(|t| s.fired < t)
                    && s.regex.as_ref().is_none_or(|r| r.is_match(subject))
            });
            if let Some(s) = hit {
                s.fired += 1;
                let r = &s.rule;
                return Some(Effect {
                    reply: r
                        .code
                        .map(|c| format!("{c} {}", r.message.as_deref().unwrap_or("Chaos rule"))),
                    delay: std::time::Duration::from_millis(r.delay_ms),
                    disconnect: r.disconnect,
                });
            }
        }
        let random = match stage {
            RuleStage::Sender => Stage::Sender,
            RuleStage::Recipient => Stage::Recipient,
            RuleStage::Auth => Stage::Authentication,
            _ => return None,
        };
        self.fire(random).map(|reply| Effect {
            reply: Some(reply),
            ..Effect::default()
        })
    }

    pub fn triggers(&self) -> Triggers {
        *self.triggers.read().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set(&self, t: Triggers) {
        *self.triggers.write().unwrap_or_else(|e| e.into_inner()) = t;
    }

    /// The reply to send instead of the normal one, if this stage fires.
    pub fn fire(&self, stage: Stage) -> Option<String> {
        let t = self.triggers().get(stage);
        if t.probability == 0 {
            return None;
        }
        let roll = getrandom::u32().unwrap_or(u32::MAX) % 100;
        (roll < u32::from(t.probability)).then(|| {
            let what = match stage {
                Stage::Sender => "sender",
                Stage::Recipient => "recipient",
                Stage::Authentication => "authentication",
            };
            format!("{} Chaos {what} error", t.error_code)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_fire() {
        let t = Triggers::parse("Sender:550:100, recipient:452:0,Auth:535:100").expect("parse");
        assert_eq!(t.sender.error_code, 550);
        assert_eq!(t.recipient.probability, 0);
        let c = Chaos::new(t);
        assert_eq!(
            c.fire(Stage::Sender).as_deref(),
            Some("550 Chaos sender error")
        );
        assert_eq!(c.fire(Stage::Recipient), None);
        assert_eq!(
            c.fire(Stage::Authentication).as_deref(),
            Some("535 Chaos authentication error")
        );
        assert!(Triggers::parse("Sender:200:1").is_err());
        assert!(Triggers::parse("Sender:450:101").is_err());
        assert!(Triggers::parse("Other:450:1").is_err());
        assert!(Triggers::parse("Sender:450").is_err());
    }

    fn rule(stage: RuleStage) -> Rule {
        Rule {
            stage,
            pattern: None,
            code: None,
            message: None,
            delay_ms: 0,
            disconnect: false,
            times: None,
        }
    }

    #[test]
    fn rules_fire_in_order_and_count_down() {
        let c = Chaos::new(Triggers::default());
        c.set_rules(vec![
            Rule {
                pattern: Some("@flaky\\.test$".into()),
                code: Some(452),
                message: Some("4.2.2 Mailbox full".into()),
                times: Some(2),
                ..rule(RuleStage::Recipient)
            },
            Rule {
                delay_ms: 1500,
                ..rule(RuleStage::Recipient)
            },
        ])
        .expect("rules");
        let full = Some("452 4.2.2 Mailbox full".to_string());
        assert_eq!(
            c.decide(RuleStage::Recipient, "a@flaky.test")
                .and_then(|e| e.reply),
            full
        );
        // A non-matching address falls through to the delay rule.
        let e = c.decide(RuleStage::Recipient, "a@ok.test").expect("delay");
        assert_eq!((e.reply, e.delay.as_millis()), (None, 1500));
        assert_eq!(
            c.decide(RuleStage::Recipient, "b@flaky.test")
                .and_then(|e| e.reply),
            full
        );
        // `times: 2` is used up: the next one gets the delay rule instead.
        assert_eq!(
            c.decide(RuleStage::Recipient, "c@flaky.test")
                .and_then(|e| e.reply),
            None
        );
        assert_eq!(c.rules()[0].1, 2);
        assert_eq!(c.decide(RuleStage::Sender, "x@y"), None);
    }

    #[test]
    fn invalid_rules_are_refused() {
        let c = Chaos::default();
        assert!(
            c.set_rules(vec![rule(RuleStage::Data)]).is_err(),
            "no effect"
        );
        let bad = |r: Rule| c.set_rules(vec![r]).expect_err("invalid");
        assert!(
            bad(Rule {
                code: Some(250),
                ..rule(RuleStage::Data)
            })
            .contains("400")
        );
        assert!(
            bad(Rule {
                code: Some(451),
                pattern: Some("(".into()),
                ..rule(RuleStage::Data)
            })
            .contains("match")
        );
        assert!(
            bad(Rule {
                code: Some(451),
                message: Some("a\r\nRSET".into()),
                ..rule(RuleStage::Data)
            })
            .contains("single line")
        );
        assert_eq!(RuleStage::parse("RCPT"), Ok(RuleStage::Recipient));
        assert!(RuleStage::parse("quit").is_err());
    }
}
