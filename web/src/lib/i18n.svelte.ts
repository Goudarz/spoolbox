// Interface language. Message content always uses dir="auto", independent of this setting.

const en = {
  inbox: 'Inbox',
  search: 'Search',
  searchPlaceholder: 'Search: from:, to:, subject:, tag:, is:unread, has:attachment…',
  clear: 'Clear',
  unread: 'unread',
  messages: 'messages',
  noMessages: 'No messages yet. Send mail to the SMTP port and it appears here.',
  noResults: 'No messages match this search.',
  markAllRead: 'Mark all read',
  deleteAll: 'Delete all',
  deleteResults: 'Delete results',
  confirmDeleteAll: 'Delete every message?',
  confirmDeleteResults: 'Delete all messages that match this search?',
  delete: 'Delete',
  release: 'Release',
  releaseTo: 'Send to (separate addresses with commas)',
  releaseSend: 'Queue for delivery',
  releaseVia: 'Through',
  releaseAllowed: 'Allowed',
  releaseBlocked: 'Blocked',
  releaseQueued: 'Queued. Delivery happens in the background; failures are retried and logged on the server.',
  markUnread: 'Mark unread',
  markRead: 'Mark read',
  download: 'Download .eml',
  back: 'Back',
  from: 'From',
  to: 'To',
  cc: 'Cc',
  bcc: 'Bcc',
  replyTo: 'Reply-To',
  date: 'Date',
  subject: 'Subject',
  noSubject: '(no subject)',
  tags: 'Tags',
  addTag: 'Add tag',
  html: 'HTML',
  text: 'Text',
  headers: 'Headers',
  raw: 'Raw',
  structure: 'Structure',
  attachments: 'Attachments',
  links: 'Links & codes',
  envelope: 'Envelope',
  noHtml: 'This message has no HTML part.',
  remoteBlocked: 'Remote images are blocked.',
  remoteLoaded: 'Remote images are shown.',
  showRemote: 'Show remote images',
  blockRemote: 'Block remote images',
  trackingPixels: 'tracking pixels',
  codes: 'Codes',
  noLinks: 'No links found.',
  checkLinks: 'Check links',
  checkingLinks: 'Checking…',
  followRedirects: 'Follow redirects',
  checkLinksNote: 'The server requests each link, which can trigger tracking or one-click unsubscribe links.',
  linkErrors: 'broken',
  linksChecked: 'links checked',
  copy: 'Copy',
  copied: 'Copied',
  live: 'Live',
  offline: 'Reconnecting…',
  loadMore: 'Load more',
  error: 'Something went wrong',
  retry: 'Retry',
  language: 'Language',
  shortcuts: 'Keyboard: j/k move, Enter open, u unread, Delete remove, / search, Esc back',
  notFound: 'Message not found.',
  envelopeFrom: 'MAIL FROM',
  envelopeTo: 'RCPT TO',
  client: 'Client',
  smtp: 'SMTP',
  noTranscript: 'No transcript. Start the server with --smtp-transcripts to record SMTP sessions.',
};

type Dict = typeof en;

const fa: Dict = {
  inbox: 'صندوق',
  search: 'جست‌وجو',
  searchPlaceholder: 'جست‌وجو: from:، to:، subject:، tag:، is:unread، has:attachment…',
  clear: 'پاک کردن',
  unread: 'خوانده‌نشده',
  messages: 'پیام',
  noMessages: 'هنوز پیامی نیست. به درگاه SMTP ایمیل بفرستید تا اینجا نمایش داده شود.',
  noResults: 'پیامی با این جست‌وجو پیدا نشد.',
  markAllRead: 'همه خوانده شد',
  deleteAll: 'حذف همه',
  deleteResults: 'حذف نتایج',
  confirmDeleteAll: 'همه پیام‌ها حذف شوند؟',
  confirmDeleteResults: 'همه پیام‌های منطبق با این جست‌وجو حذف شوند؟',
  delete: 'حذف',
  release: 'ارسال به بیرون',
  releaseTo: 'ارسال به (آدرس‌ها را با ویرگول جدا کنید)',
  releaseSend: 'قرار دادن در صف ارسال',
  releaseVia: 'از طریق',
  releaseAllowed: 'مجاز',
  releaseBlocked: 'مسدود',
  releaseQueued: 'در صف قرار گرفت. ارسال در پس‌زمینه انجام می‌شود و خطاها روی سرور دوباره تلاش و ثبت می‌شوند.',
  markUnread: 'خوانده‌نشده',
  markRead: 'خوانده‌شده',
  download: 'دانلود ‎.eml',
  back: 'بازگشت',
  from: 'از',
  to: 'به',
  cc: 'رونوشت',
  bcc: 'رونوشت مخفی',
  replyTo: 'پاسخ به',
  date: 'تاریخ',
  subject: 'موضوع',
  noSubject: '(بدون موضوع)',
  tags: 'برچسب‌ها',
  addTag: 'افزودن برچسب',
  html: 'HTML',
  text: 'متن',
  headers: 'هدرها',
  raw: 'خام',
  structure: 'ساختار',
  attachments: 'پیوست‌ها',
  links: 'لینک‌ها و کدها',
  envelope: 'پاکت',
  noHtml: 'این پیام بخش HTML ندارد.',
  remoteBlocked: 'تصاویر خارجی مسدود است.',
  remoteLoaded: 'تصاویر خارجی نمایش داده می‌شود.',
  showRemote: 'نمایش تصاویر خارجی',
  blockRemote: 'مسدود کردن تصاویر خارجی',
  trackingPixels: 'پیکسل ردیابی',
  codes: 'کدها',
  noLinks: 'لینکی پیدا نشد.',
  checkLinks: 'بررسی لینک‌ها',
  checkingLinks: 'در حال بررسی…',
  followRedirects: 'دنبال کردن redirect',
  checkLinksNote: 'سرور به هر لینک درخواست می‌فرستد و این کار ممکن است لینک‌های ردیابی یا لغو عضویت را فعال کند.',
  linkErrors: 'خراب',
  linksChecked: 'لینک بررسی شد',
  copy: 'کپی',
  copied: 'کپی شد',
  live: 'زنده',
  offline: 'در حال اتصال دوباره…',
  loadMore: 'بیشتر',
  error: 'خطایی رخ داد',
  retry: 'تلاش دوباره',
  language: 'زبان',
  shortcuts: 'صفحه‌کلید: j/k جابه‌جایی، Enter باز کردن، u خوانده‌نشده، Delete حذف، / جست‌وجو، Esc بازگشت',
  notFound: 'پیام پیدا نشد.',
  envelopeFrom: 'MAIL FROM',
  envelopeTo: 'RCPT TO',
  client: 'کلاینت',
  smtp: 'SMTP',
  noTranscript: 'گفت‌وگوی SMTP ثبت نشده است. سرور را با --smtp-transcripts اجرا کنید.',
};

export const languages = { en: 'English', fa: 'فارسی' } as const;
export type Lang = keyof typeof languages;
const dicts: Record<Lang, Dict> = { en, fa };

function initial(): Lang {
  try {
    const saved = localStorage.getItem('spoolbox.lang');
    if (saved === 'en' || saved === 'fa') return saved;
  } catch {
    // Storage may be unavailable.
  }
  return navigator.language.toLowerCase().startsWith('fa') ? 'fa' : 'en';
}

export const locale = $state({ lang: initial() });

export const t = (key: keyof Dict): string => dicts[locale.lang][key];

export function setLang(lang: Lang) {
  locale.lang = lang;
  try {
    localStorage.setItem('spoolbox.lang', lang);
  } catch {
    // Not persisted; still applied for this page.
  }
}

export const dir = (): 'rtl' | 'ltr' => (locale.lang === 'fa' ? 'rtl' : 'ltr');

export function formatDate(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return new Intl.DateTimeFormat(locale.lang === 'fa' ? 'fa-IR' : undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(d);
}

export function formatSize(bytes: number): string {
  const units = ['B', 'KiB', 'MiB', 'GiB'];
  let n = bytes;
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${i === 0 ? n : n.toFixed(1)} ${units[i]}`;
}
