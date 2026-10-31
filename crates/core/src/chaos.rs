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

/// Shared between the SMTP sessions and the HTTP API.
#[derive(Debug, Default)]
pub struct Chaos {
    triggers: RwLock<Triggers>,
}

impl Chaos {
    pub fn new(triggers: Triggers) -> Self {
        Self {
            triggers: RwLock::new(triggers),
        }
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
}
