//! v1 JSON shapes. Empty `To`/`Cc`/`Bcc` are `null` in list entries but `[]` in the full
//! message; clients of the v1 API rely on this.

use serde::Serialize;
use spoolbox_store::{MessageRow, Part, Recipient};

use crate::api::rfc3339;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Address {
    pub name: String,
    pub address: String,
}

impl From<&spoolbox_mime::Address> for Address {
    fn from(a: &spoolbox_mime::Address) -> Self {
        Self {
            name: a.name.clone(),
            address: a.addr.clone(),
        }
    }
}

/// Header recipients plus envelope recipients that appear in no header (shown as Bcc, as
/// v1 clients expect).
#[derive(Debug, Default)]
pub(crate) struct Recipients {
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
}

impl From<&Vec<Recipient>> for Recipients {
    fn from(list: &Vec<Recipient>) -> Self {
        let mut out = Recipients::default();
        let mut envelope = Vec::new();
        for r in list {
            let a = Address::from(&r.address);
            match r.kind.as_str() {
                "to" => out.to.push(a),
                "cc" => out.cc.push(a),
                "bcc" => out.bcc.push(a),
                "reply_to" => out.reply_to.push(a),
                "envelope" => envelope.push(a),
                _ => {}
            }
        }
        for e in envelope {
            let known = out
                .to
                .iter()
                .chain(&out.cc)
                .chain(&out.bcc)
                .any(|a| a.address.eq_ignore_ascii_case(&e.address));
            if !known {
                out.bcc.push(Address {
                    name: String::new(),
                    address: e.address,
                });
            }
        }
        out
    }
}

fn from_address(m: &MessageRow) -> Option<Address> {
    (!m.from.addr.is_empty() || !m.from.name.is_empty()).then(|| Address::from(&m.from))
}

fn non_empty(v: &[Address]) -> Option<Vec<Address>> {
    (!v.is_empty()).then(|| v.to_vec())
}

#[derive(Debug, Serialize)]
pub(crate) struct Summary {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "MessageID")]
    pub message_id: String,
    #[serde(rename = "Read")]
    pub read: bool,
    #[serde(rename = "From")]
    pub from: Option<Address>,
    #[serde(rename = "To")]
    pub to: Option<Vec<Address>>,
    #[serde(rename = "Cc")]
    pub cc: Option<Vec<Address>>,
    #[serde(rename = "Bcc")]
    pub bcc: Option<Vec<Address>>,
    #[serde(rename = "ReplyTo")]
    pub reply_to: Option<Vec<Address>>,
    #[serde(rename = "Subject")]
    pub subject: String,
    #[serde(rename = "Created")]
    pub created: String,
    #[serde(rename = "Username")]
    pub username: String,
    #[serde(rename = "Tags")]
    pub tags: Vec<String>,
    #[serde(rename = "Size")]
    pub size: u64,
    #[serde(rename = "Attachments")]
    pub attachments: u32,
    #[serde(rename = "Snippet")]
    pub snippet: String,
}

impl Summary {
    /// With `search`, an empty `ReplyTo` is `null` (search results); lists use `[]`.
    pub fn new(m: &MessageRow, r: &Recipients, username: String, search: bool) -> Self {
        Self {
            id: m.id.clone(),
            message_id: m.message_id.clone().unwrap_or_default(),
            read: m.read,
            from: from_address(m),
            to: non_empty(&r.to),
            cc: non_empty(&r.cc),
            bcc: non_empty(&r.bcc),
            reply_to: if search {
                non_empty(&r.reply_to)
            } else {
                Some(r.reply_to.clone())
            },
            subject: m.subject.clone(),
            created: rfc3339(m.received_at),
            username,
            tags: m.tags.clone(),
            size: m.size,
            attachments: m.attachment_count,
            snippet: m.snippet.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct MessagesSummary {
    pub total: u64,
    pub unread: u64,
    /// Legacy: entries in this page.
    pub count: u64,
    pub messages_count: u64,
    pub messages_unread: u64,
    pub start: usize,
    pub tags: Vec<String>,
    pub messages: Vec<Summary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Checksums {
    #[serde(rename = "MD5")]
    pub md5: String,
    #[serde(rename = "SHA1")]
    pub sha1: String,
    #[serde(rename = "SHA256")]
    pub sha256: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct Attachment {
    #[serde(rename = "PartID")]
    pub part_id: String,
    #[serde(rename = "FileName")]
    pub file_name: String,
    #[serde(rename = "ContentType")]
    pub content_type: String,
    #[serde(rename = "ContentID")]
    pub content_id: String,
    #[serde(rename = "Size")]
    pub size: u64,
    #[serde(rename = "Checksums")]
    pub checksums: Checksums,
}

impl From<&Part> for Attachment {
    fn from(p: &Part) -> Self {
        Self {
            part_id: p.part_id.clone(),
            file_name: p.filename.clone().unwrap_or_default(),
            content_type: p.content_type.clone(),
            content_id: p.content_id.clone().unwrap_or_default(),
            size: p.decoded_size,
            checksums: Checksums {
                md5: p.md5.clone().unwrap_or_default(),
                sha1: p.sha1.clone().unwrap_or_default(),
                sha256: p.sha256.clone().unwrap_or_default(),
            },
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ListUnsubscribe {
    pub header: String,
    pub links: Vec<String>,
    pub errors: String,
    pub header_post: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct Message {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "MessageID")]
    pub message_id: String,
    #[serde(rename = "From")]
    pub from: Option<Address>,
    #[serde(rename = "To")]
    pub to: Vec<Address>,
    #[serde(rename = "Cc")]
    pub cc: Vec<Address>,
    #[serde(rename = "Bcc")]
    pub bcc: Vec<Address>,
    #[serde(rename = "ReplyTo")]
    pub reply_to: Vec<Address>,
    #[serde(rename = "ReturnPath")]
    pub return_path: String,
    #[serde(rename = "Subject")]
    pub subject: String,
    #[serde(rename = "ListUnsubscribe")]
    pub list_unsubscribe: ListUnsubscribe,
    #[serde(rename = "Date")]
    pub date: String,
    #[serde(rename = "Tags")]
    pub tags: Vec<String>,
    #[serde(rename = "Username")]
    pub username: String,
    #[serde(rename = "Text")]
    pub text: String,
    #[serde(rename = "HTML")]
    pub html: String,
    #[serde(rename = "Size")]
    pub size: u64,
    #[serde(rename = "Inline")]
    pub inline: Vec<Attachment>,
    #[serde(rename = "Attachments")]
    pub attachments: Vec<Attachment>,
}

pub(crate) fn from_row(m: &MessageRow) -> Option<Address> {
    from_address(m)
}
