// Shapes of the API v1 and v2 responses used by the UI (see api/openapi-v1.json, api/openapi-v2.json).

export interface Address {
  Name: string;
  Address: string;
}

export interface Summary {
  ID: string;
  MessageID: string;
  Read: boolean;
  From: Address | null;
  To: Address[] | null;
  Cc: Address[] | null;
  Bcc: Address[] | null;
  ReplyTo: Address[] | null;
  Subject: string;
  Created: string;
  Username: string;
  Tags: string[];
  Size: number;
  Attachments: number;
  Snippet: string;
}

export interface MessagesPage {
  total: number;
  unread: number;
  count: number;
  messages_count: number;
  messages_unread: number;
  start: number;
  tags: string[];
  messages: Summary[];
}

export interface Attachment {
  PartID: string;
  FileName: string;
  ContentType: string;
  ContentID: string;
  Size: number;
}

export interface Message {
  ID: string;
  MessageID: string;
  From: Address | null;
  To: Address[];
  Cc: Address[];
  Bcc: Address[];
  ReplyTo: Address[];
  ReturnPath: string;
  Subject: string;
  ListUnsubscribe: { Header: string; Links: string[]; Errors: string; HeaderPost: string };
  Date: string;
  Tags: string[];
  Username: string;
  Text: string;
  HTML: string;
  Size: number;
  Inline: Attachment[];
  Attachments: Attachment[];
}

export interface WebUi {
  Label: string;
  ChaosEnabled: boolean;
  DuplicatesIgnored: boolean;
  HideDeleteAllButton: boolean;
}

export interface MimePart {
  part_id: string;
  content_type: string;
  charset: string | null;
  disposition: string | null;
  filename: string | null;
  content_id: string | null;
  content_language: string | null;
  transfer_encoding: string | null;
  size: number;
  children?: MimePart[];
}

export interface Link {
  url: string;
  text: string | null;
  source: 'html_a' | 'html_img' | 'html_css' | 'text';
  tracking_pixel: boolean;
}

export interface Code {
  value: string;
  source: 'subject' | 'text' | 'html';
}

export interface Envelope {
  mail_from: string;
  mail_params: Record<string, string>;
  rcpt_to: string[];
  helo: string;
  remote_ip: string;
  tls: string | null;
  auth_user: string | null;
  source: string;
}

/** WebSocket notification from /api/events. */
export type Notification =
  | { Type: 'new'; Data: Summary }
  | { Type: 'update'; Data: { ID: string; Read?: boolean; Tags?: string[] } }
  | { Type: 'delete'; Data: { ID: string } }
  | { Type: 'truncate' | 'prune'; Data: null }
  | { Type: 'stats'; Data: { Total: number; Unread: number; Version: string } };
