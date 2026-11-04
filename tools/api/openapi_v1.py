# Builds api/openapi-v1.json (OpenAPI 3.1) for Spoolbox's API v1.
#   python tools/api/openapi_v1.py api/openapi-v1.json
# CI regenerates the file and fails if it differs from the committed one.
import json, sys

out = sys.argv[1] if len(sys.argv) > 1 else "api/openapi-v1.json"


def ref(name):
    return {"$ref": f"#/components/schemas/{name}"}


def obj(props, required=None, **extra):
    o = {"type": "object", "properties": props}
    if required:
        o["required"] = required
    o.update(extra)
    return o


S = {"type": "string"}
I = {"type": "integer"}
B = {"type": "boolean"}
NULLABLE_ADDRS = {"type": ["array", "null"], "items": ref("Address"), "description": "null when empty"}
ADDRS = {"type": "array", "items": ref("Address")}
STRINGS = {"type": "array", "items": S}

schemas = {
    "Address": obj({"Name": S, "Address": S}, ["Name", "Address"]),
    "MessageSummary": obj({
        "ID": S, "MessageID": S, "Read": B, "From": {"oneOf": [ref("Address"), {"type": "null"}]},
        "To": NULLABLE_ADDRS, "Cc": NULLABLE_ADDRS,
        "Bcc": dict(NULLABLE_ADDRS, description="Bcc header plus envelope recipients missing from To/Cc/Bcc; null when empty"),
        "ReplyTo": {"type": ["array", "null"], "items": ref("Address"), "description": "[] when empty in lists, null in search results"},
        "Subject": S, "Created": dict(S, format="date-time"), "Username": S, "Tags": STRINGS,
        "Size": dict(I, description="Bytes of the message as received"), "Attachments": I, "Snippet": S,
    }),
    "MessagesSummary": obj({
        "total": dict(I, description="Messages in the mailbox"), "unread": dict(I, description="Unread messages in the mailbox"),
        "count": dict(I, description="Entries in this page (legacy)"),
        "messages_count": dict(I, description="List: all messages; search: matching messages"),
        "messages_unread": dict(I, description="List: unread messages; search: matching unread messages"),
        "start": I, "tags": STRINGS, "messages": {"type": "array", "items": ref("MessageSummary")},
    }),
    "Checksums": obj({"MD5": S, "SHA1": S, "SHA256": S}),
    "Attachment": obj({"PartID": S, "FileName": S, "ContentType": S, "ContentID": S, "Size": I, "Checksums": ref("Checksums")}),
    "ListUnsubscribe": obj({"Header": S, "Links": STRINGS, "Errors": S, "HeaderPost": S}),
    "Message": obj({
        "ID": S, "MessageID": S, "From": {"oneOf": [ref("Address"), {"type": "null"}]},
        "To": ADDRS, "Cc": ADDRS, "Bcc": ADDRS, "ReplyTo": ADDRS,
        "ReturnPath": dict(S, description="Envelope sender, else the Return-Path header, else From"),
        "Subject": S, "ListUnsubscribe": ref("ListUnsubscribe"), "Date": dict(S, format="date-time"),
        "Tags": STRINGS, "Username": dict(S, description="SMTP AUTH or HTTP Basic user name"),
        "Text": S, "HTML": dict(S, description="As received; `cid:` links are rewritten only by /view/{id}.html"),
        "Size": I, "Inline": {"type": "array", "items": ref("Attachment")},
        "Attachments": {"type": "array", "items": ref("Attachment")},
    }),
    "Headers": {"type": "object", "additionalProperties": STRINGS,
                "description": "Header name (canonical form, e.g. `Message-Id`) to raw values, in order"},
    "SetRead": obj({"Read": B, "IDs": STRINGS, "Search": dict(S, description="Search query; not together with IDs")}),
    "DeleteMessages": obj({"IDs": dict(STRINGS, description="Empty or missing deletes every message")}),
    "SetTags": obj({"IDs": STRINGS, "Tags": dict(STRINGS, description="Replaces the tags; empty removes all")}),
    "RenameTag": obj({"Name": S}, ["Name"]),
    "SendAddress": {"oneOf": [obj({"Email": S, "Name": S}, ["Email"]), dict(S, description="`a@b` or `Name <a@b>`")]},
    "SendAttachment": obj({"Content": dict(S, description="Base64, optionally as a data: URL"), "Filename": S,
                           "ContentType": dict(S, description="Detected when empty"),
                           "ContentID": dict(S, description="Makes the attachment inline")}, ["Content", "Filename"]),
    "SendRequest": obj({
        "From": ref("SendAddress"), "To": {"type": "array", "items": ref("SendAddress")},
        "Cc": {"type": "array", "items": ref("SendAddress")}, "Bcc": {"type": "array", "items": S},
        "ReplyTo": {"type": "array", "items": ref("SendAddress")}, "Subject": S, "Text": S, "HTML": S,
        "Attachments": {"type": "array", "items": ref("SendAttachment")},
        "Headers": {"type": "object", "additionalProperties": S,
                    "description": "Not allowed: To, From, Cc, Bcc, Reply-To, Date, Subject, Content-Type, Mime-Version (and X-Tags with Tags)"},
        "Tags": STRINGS,
    }, ["From"]),
    "SendResponse": obj({"ID": S}),
    "SendError": obj({"Error": S}),
    "Trigger": obj({"ErrorCode": dict(I, minimum=400, maximum=599), "Probability": dict(I, minimum=0, maximum=100)}),
    "ChaosTriggers": obj({"Sender": ref("Trigger"), "Recipient": ref("Trigger"), "Authentication": ref("Trigger")}),
    "AppInformation": obj({
        "Version": S, "LatestVersion": dict(S, description="Always `disabled`: no update check"),
        "Database": S, "DatabaseSize": I, "Messages": I, "Unread": I,
        "Tags": {"type": "object", "additionalProperties": I},
        "RuntimeStats": obj({"Uptime": I, "Memory": I, "MessagesDeleted": I, "SMTPAccepted": I,
                             "SMTPAcceptedSize": I, "SMTPRejected": I, "SMTPIgnored": I}),
    }),
    "WebUIConfiguration": obj({
        "Label": S,
        "MessageRelay": obj({"Enabled": B, "SMTPServer": S, "ReturnPath": S, "AllowedRecipients": S,
                             "BlockedRecipients": S, "OverrideFrom": S, "PreserveMessageIDs": B, "RecipientAllowlist": S}),
        "SpamAssassin": B, "ChaosEnabled": B, "DuplicatesIgnored": B, "HideDeleteAllButton": B,
    }),
}

TEXT = {"text/plain": {"schema": S}}


def json_resp(desc, schema):
    return {"description": desc, "content": {"application/json": {"schema": schema}}}


OK = {"description": "`ok`", "content": TEXT}
BAD = {"description": "Invalid request", "content": TEXT}
NOT_FOUND = {"description": "Not found", "content": TEXT}
ID = {"name": "id", "in": "path", "required": True, "schema": S, "description": "Message ID, or `latest` (optionally filtered with `query`)"}
PART = {"name": "part", "in": "path", "required": True, "schema": S, "description": "MIME part number, e.g. `1.2`"}
LATEST_QUERY = {"name": "query", "in": "query", "schema": S, "description": "With `latest`: newest message matching this search"}
PAGE = [
    {"name": "start", "in": "query", "schema": dict(I, default=0)},
    {"name": "limit", "in": "query", "schema": dict(I, default=50), "description": "0 = no limit"},
    {"name": "before", "in": "query", "schema": S, "description": "Only messages received before this date/time"},
    {"name": "tz", "in": "query", "schema": S, "description": "Time zone for dates without offset (IANA name or +03:30)"},
]
QUERY = {"name": "query", "in": "query", "required": True, "schema": S, "description": "Search query"}
TZ = {"name": "tz", "in": "query", "schema": S}


def op(op_id, summary, tag, responses, params=None, body=None, desc=None):
    o = {"operationId": op_id, "summary": summary, "tags": [tag], "responses": responses}
    if desc:
        o["description"] = desc
    if params:
        o["parameters"] = params
    if body:
        o["requestBody"] = {"required": True, "content": {"application/json": {"schema": body}}}
    return o


paths = {
    "/api/v1/messages": {
        "get": op("listMessages", "List messages, newest first", "messages", {"200": json_resp("Page of messages", ref("MessagesSummary"))}, PAGE),
        "put": op("setReadStatus", "Mark messages read or unread", "messages", {"200": OK, "400": BAD}, [TZ], ref("SetRead"),
                  "By IDs, by search, or (neither) every message."),
        "delete": op("deleteMessages", "Delete messages", "messages", {"200": OK, "400": BAD}, None, ref("DeleteMessages"),
                     "An empty body or empty `IDs` deletes every message; a body that is not valid JSON is rejected."),
    },
    "/api/v1/search": {
        "get": op("search", "Search messages", "messages", {"200": json_resp("Page of matching messages", ref("MessagesSummary")), "400": BAD}, [QUERY] + PAGE),
        "delete": op("deleteSearch", "Delete matching messages", "messages", {"200": OK, "400": BAD}, [QUERY, TZ]),
    },
    "/api/v1/message/{id}": {"get": op("getMessage", "Message summary and bodies (marks it read)", "message",
                                       {"200": json_resp("Message", ref("Message")), "404": NOT_FOUND}, [ID, LATEST_QUERY])},
    "/api/v1/message/{id}/headers": {"get": op("getHeaders", "Message headers", "message",
                                               {"200": json_resp("Headers", ref("Headers")), "404": NOT_FOUND}, [ID, LATEST_QUERY])},
    "/api/v1/message/{id}/raw": {"get": op("getRaw", "The message exactly as received", "message",
                                           {"200": {"description": "Raw message", "content": TEXT}, "404": NOT_FOUND},
                                           [ID, LATEST_QUERY, {"name": "dl", "in": "query", "schema": S, "description": "`1` to download as `{id}.eml`"}])},
    "/api/v1/message/{id}/part/{part}": {"get": op("getPart", "Decoded MIME part", "message",
                                                   {"200": {"description": "Part content; HTML, XML and SVG are served as attachment/octet-stream",
                                                            "content": {"application/octet-stream": {}}}, "404": NOT_FOUND}, [ID, PART])},
    "/api/v1/message/{id}/part/{part}/thumb": {"get": op("getThumbnail", "180×120 JPEG thumbnail (white for non-images)", "message",
                                                         {"200": {"description": "JPEG", "content": {"image/jpeg": {}}}, "400": BAD},
                                                         [dict(ID, description="Message ID"), PART])},
    "/api/v1/message/{id}/release": {"post": op("releaseMessage", "Queue the message for the configured relay server; delivery is retried in the background", "message",
                                                {"200": OK, "400": BAD, "404": NOT_FOUND}, [ID], obj({"To": STRINGS}, ["To"]))},
    "/api/v1/message/{id}/html-check": {"get": op("htmlCheck", "HTML compatibility check (not available yet)", "message",
                                                  {"501": {"description": "Not implemented", "content": TEXT}}, [ID])},
    "/api/v1/message/{id}/link-check": {"get": op("linkCheck", "Link check (not available yet)", "message",
                                                  {"501": {"description": "Not implemented", "content": TEXT}}, [ID])},
    "/api/v1/tags": {
        "get": op("getTags", "All tags", "tags", {"200": json_resp("Tag names", STRINGS)}),
        "put": op("setTags", "Replace the tags of messages", "tags", {"200": OK, "400": BAD}, None, ref("SetTags")),
    },
    "/api/v1/tags/{tag}": {
        "put": op("renameTag", "Rename a tag", "tags", {"200": OK, "400": BAD},
                  [{"name": "tag", "in": "path", "required": True, "schema": S}], ref("RenameTag")),
        "delete": op("deleteTag", "Delete a tag", "tags", {"200": OK},
                     [{"name": "tag", "in": "path", "required": True, "schema": S}]),
    },
    "/api/v1/send": {"post": op("sendMessage", "Store a message built from JSON", "send",
                                {"200": json_resp("Stored", ref("SendResponse")), "400": json_resp("Invalid request", ref("SendError")),
                                 "401": {"description": "Credentials required"}, "413": json_resp("Too large", ref("SendError"))},
                                None, ref("SendRequest"),
                                "Uses the send API credentials when configured, otherwise the web UI credentials.")},
    "/api/v1/info": {"get": op("appInfo", "Instance information", "application", {"200": json_resp("Information", ref("AppInformation"))})},
    "/api/v1/webui": {"get": op("webUiConfig", "Web UI configuration", "application", {"200": json_resp("Configuration", ref("WebUIConfiguration"))})},
    "/api/v1/chaos": {
        "get": op("getChaos", "Current chaos triggers", "testing", {"200": json_resp("Triggers", ref("ChaosTriggers")), "400": BAD}),
        "put": op("setChaos", "Set chaos triggers (omitted ones are reset)", "testing",
                  {"200": json_resp("Triggers", ref("ChaosTriggers")), "400": BAD}, None, ref("ChaosTriggers")),
    },
    "/api/v1/swagger.json": {"get": op("openapi", "This document", "application", {"200": json_resp("OpenAPI document", {"type": "object"})})},
    "/view/{id}.html": {"get": op("viewHtml", "HTML body for display (marks it read)", "view",
                                  {"200": {"description": "HTML with `cid:` links rewritten, served with a sandboxing CSP",
                                           "content": {"text/html": {"schema": S}}}, "404": NOT_FOUND},
                                  [ID, LATEST_QUERY, {"name": "embed", "in": "query", "schema": S,
                                                      "description": "`1`: links open in a new tab; the page posts its height to the parent"}])},
    "/view/{id}.txt": {"get": op("viewText", "Text body (marks it read)", "view",
                                 {"200": {"description": "Text", "content": TEXT}, "404": NOT_FOUND}, [ID, LATEST_QUERY])},
    "/api/events": {"get": op("events", "WebSocket notifications", "events", {"101": {"description": "Switching protocols"}},
                              desc="Text frames `{\"Type\", \"Data\"}`; types: `new` (MessageSummary), `update` (`{ID, Read}` or `{ID, Tags}`), "
                                   "`delete` (`{ID}`), `truncate`, `prune` (reload), `stats` (`{Total, Unread, Version}`).")},
    "/livez": {"get": op("livez", "Liveness", "health", {"200": {"description": "Alive"}})},
    "/readyz": {"get": op("readyz", "Readiness", "health", {"200": {"description": "Ready"}, "503": {"description": "Starting or stopping"}})},
}

doc = {
    "openapi": "3.1.0",
    "info": {
        "title": "Spoolbox API v1",
        "version": "v1",
        "description": "Stable API used by the web UI and test clients. JSON field names are PascalCase (list summaries use snake_case). "
                       "Errors are `text/plain` with status 400 (send API: JSON `{\"Error\"}`). Prefix every path with --webroot when set.",
        "license": {"name": "MIT", "identifier": "MIT"},
    },
    "servers": [{"url": "/", "description": "Prefix with --webroot when set"}],
    "security": [{}, {"basicAuth": []}],
    "paths": paths,
    "components": {"securitySchemes": {"basicAuth": {"type": "http", "scheme": "basic"}}, "schemas": schemas},
}
with open(out, "w", encoding="utf-8", newline="\n") as f:
    json.dump(doc, f, ensure_ascii=False, indent=2)
    f.write("\n")
print("wrote", out)
