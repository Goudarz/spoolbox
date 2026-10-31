//! htpasswd-style credentials, as accepted by the `--*-auth-file` options and the
//! `MP_*_AUTH` variables (whitespace-separated `user:password` entries).
//!
//! Supported password formats: bcrypt (`$2a$`, `$2b$`, `$2y$`), SHA-crypt (`$5$`, `$6$`),
//! Apache MD5 (`$apr1$`) and MD5-crypt (`$1$`), `{SHA}`, `{SSHA}`, and plaintext.

use std::collections::HashMap;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use md5::{Digest, Md5};
use sha1::Sha1;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("cannot read password file {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid credentials entry on line {line}: expected user:password")]
    Syntax { line: usize },
    #[error("no credentials found")]
    Empty,
}

#[derive(Clone, PartialEq, Eq)]
enum Hash {
    Bcrypt(String),
    ShaCrypt(String),
    Md5Crypt {
        magic: &'static str,
        salt: String,
        full: String,
    },
    Sha1([u8; 20]),
    Ssha {
        digest: [u8; 20],
        salt: Vec<u8>,
    },
    Plain(String),
}

/// A set of users and their password hashes. `Debug` never shows hashes.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Credentials {
    users: HashMap<String, Hash>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("users", &self.users.len())
            .finish()
    }
}

impl Credentials {
    /// Parses htpasswd text: one `user:hash` per line or whitespace-separated (`MP_*_AUTH` style).
    pub fn parse(text: &str) -> Result<Self, AuthError> {
        let mut users = HashMap::new();
        let entries = text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim_start().starts_with('#'))
            .flat_map(|(n, l)| l.split_whitespace().map(move |e| (n + 1, e)));
        for (n, entry) in entries {
            let (user, secret) = entry.split_once(':').ok_or(AuthError::Syntax { line: n })?;
            if user.is_empty() {
                return Err(AuthError::Syntax { line: n });
            }
            users.insert(user.to_string(), classify(secret));
        }
        if users.is_empty() {
            return Err(AuthError::Empty);
        }
        Ok(Self { users })
    }

    pub fn from_file(path: &Path) -> Result<Self, AuthError> {
        let text = std::fs::read_to_string(path).map_err(|source| AuthError::Read {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&text)
    }

    pub fn len(&self) -> usize {
        self.users.len()
    }

    pub fn is_empty(&self) -> bool {
        self.users.is_empty()
    }

    /// Checks a username and password. Unknown users take the same code path as wrong passwords.
    pub fn verify(&self, user: &str, password: &str) -> bool {
        let Some(hash) = self.users.get(user) else {
            // Burn comparable time so user enumeration by timing is harder.
            let _ = constant_eq(password.as_bytes(), b"spoolbox-dummy-password");
            return false;
        };
        let pw = password.as_bytes();
        match hash {
            Hash::Bcrypt(h) => bcrypt::verify(pw, h).unwrap_or(false),
            Hash::ShaCrypt(h) => {
                use sha_crypt::PasswordVerifier;
                sha_crypt::ShaCrypt::default()
                    .verify_password(pw, h.as_str())
                    .is_ok()
            }
            Hash::Md5Crypt { magic, salt, full } => constant_eq(
                md5_crypt(pw, salt.as_bytes(), magic).as_bytes(),
                full.as_bytes(),
            ),
            Hash::Sha1(d) => constant_eq(&Sha1::digest(pw), d),
            Hash::Ssha { digest, salt } => {
                let mut h = Sha1::new();
                h.update(pw);
                h.update(salt);
                constant_eq(&h.finalize(), digest)
            }
            Hash::Plain(p) => constant_eq(pw, p.as_bytes()),
        }
    }
}

fn classify(secret: &str) -> Hash {
    if secret.starts_with("$2a$") || secret.starts_with("$2b$") || secret.starts_with("$2y$") {
        return Hash::Bcrypt(secret.to_string());
    }
    if secret.starts_with("$5$") || secret.starts_with("$6$") {
        return Hash::ShaCrypt(secret.to_string());
    }
    for magic in ["$apr1$", "$1$"] {
        if let Some(rest) = secret.strip_prefix(magic)
            && let Some((salt, _)) = rest.split_once('$')
        {
            return Hash::Md5Crypt {
                magic,
                salt: salt.to_string(),
                full: secret.to_string(),
            };
        }
    }
    if let Some(b) = secret
        .strip_prefix("{SHA}")
        .and_then(|b| B64.decode(b).ok())
        && let Ok(d) = <[u8; 20]>::try_from(b.as_slice())
    {
        return Hash::Sha1(d);
    }
    if let Some(b) = secret
        .strip_prefix("{SSHA}")
        .and_then(|b| B64.decode(b).ok())
        && b.len() > 20
    {
        let mut digest = [0u8; 20];
        digest.copy_from_slice(&b[..20]);
        return Hash::Ssha {
            digest,
            salt: b[20..].to_vec(),
        };
    }
    Hash::Plain(secret.to_string())
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

const ITOA64: &[u8; 64] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// MD5-crypt (`$1$`, Poul-Henning Kamp) and Apache's variant (`$apr1$`), which differs only in the magic string.
fn md5_crypt(pw: &[u8], salt: &[u8], magic: &str) -> String {
    let salt = &salt[..salt.len().min(8)];
    let alt = Md5::new()
        .chain_update(pw)
        .chain_update(salt)
        .chain_update(pw)
        .finalize();
    let mut ctx = Md5::new()
        .chain_update(pw)
        .chain_update(magic.as_bytes())
        .chain_update(salt);
    let mut left = pw.len();
    while left > 0 {
        let n = left.min(16);
        ctx.update(&alt[..n]);
        left -= n;
    }
    let mut i = pw.len();
    while i > 0 {
        if i & 1 == 1 {
            ctx.update([0u8]);
        } else {
            ctx.update(&pw[..1]);
        }
        i >>= 1;
    }
    let mut fin = ctx.finalize();
    for round in 0..1000 {
        let mut c = Md5::new();
        if round & 1 == 1 {
            c.update(pw);
        } else {
            c.update(fin);
        }
        if round % 3 != 0 {
            c.update(salt);
        }
        if round % 7 != 0 {
            c.update(pw);
        }
        if round & 1 == 1 {
            c.update(fin);
        } else {
            c.update(pw);
        }
        fin = c.finalize();
    }
    let mut out = format!("{magic}{}$", String::from_utf8_lossy(salt));
    let mut push = |v: u32, n: usize| {
        let mut v = v;
        for _ in 0..n {
            out.push(ITOA64[(v & 0x3f) as usize] as char);
            v >>= 6;
        }
    };
    for (a, b, c) in [(0, 6, 12), (1, 7, 13), (2, 8, 14), (3, 9, 15), (4, 10, 5)] {
        push(
            (u32::from(fin[a]) << 16) | (u32::from(fin[b]) << 8) | u32::from(fin[c]),
            4,
        );
    }
    push(u32::from(fin[11]), 2);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PW: &str = "p@ss wörd";

    #[test]
    fn openssl_vectors() {
        // Generated with OpenSSL 3.2.1 `openssl passwd`.
        for hash in [
            "$apr1$saltsalt$S6D2MtcWGkSu2W25x9Erb.",
            "$1$saltsalt$/GLr6X2dnS0ia2gvrA/Qg0",
            "$5$saltsalt$r9UqAdmng8m1HYhHhcrllKcKfe1GfAfkWztzPO0JS/8",
            "$6$saltsalt$SFRaKnCT4od8vzOox5akmgot.pwmob2w0tVGdN06gV5VZs/v69UzCXuaGilC7TepAl6hl8Otr0oB1QIuAhgAu/",
            "{SHA}iFXrzdzW6SkQ4l5UHiG/hH6VbME=",
            "{SSHA}cFIEs9qXfZW1wl9nOUp3rwTZc3JOYUNs",
        ] {
            // Entries cannot contain spaces in whitespace-separated form; use one per line.
            let c = Credentials::parse(&format!("user:{hash}\n")).expect("parse");
            assert!(c.verify("user", PW), "{hash}");
            assert!(!c.verify("user", "wrong"), "{hash}");
        }
        assert_eq!(
            md5_crypt(b"x", b"ab", "$apr1$"),
            "$apr1$ab$eIePjsejfBGR8ITtu2z0U1"
        );
    }

    #[test]
    fn bcrypt_round_trip() {
        let hash = bcrypt::hash("secret", 4).expect("hash");
        let c = Credentials::parse(&format!("alice:{hash}")).expect("parse");
        assert!(c.verify("alice", "secret"));
        assert!(!c.verify("alice", "Secret"));
    }

    #[test]
    fn inline_style_and_plaintext() {
        let c = Credentials::parse("user1:password1 user2:password2\n# comment\n").expect("parse");
        assert_eq!(c.len(), 2);
        assert!(c.verify("user2", "password2"));
        assert!(!c.verify("user3", "password2"));
        assert!(!c.verify("user1", "password1x"));
    }

    #[test]
    fn errors_and_debug() {
        assert!(matches!(Credentials::parse("   "), Err(AuthError::Empty)));
        assert!(matches!(
            Credentials::parse("nouser"),
            Err(AuthError::Syntax { line: 1 })
        ));
        assert!(matches!(
            Credentials::parse(":pw"),
            Err(AuthError::Syntax { .. })
        ));
        let c = Credentials::parse("a:secret").expect("parse");
        assert!(!format!("{c:?}").contains("secret"));
        assert!(Credentials::from_file(Path::new("does-not-exist")).is_err());
    }
}
