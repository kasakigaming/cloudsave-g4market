// Phần vẽ của giao diện mặc định (phẳng, tối, một màu nhấn lime).
//
// Toàn bộ là hàm thuần: nhận dữ liệu, trả về HTML. Trạng thái và hành động
// vẫn nằm ở main.ts — hai giao diện dùng chung đúng một bộ dữ liệu và một bộ
// thuộc tính `data-*` cho nút (`data-push`, `data-restore-local`…), nên đổi
// giao diện không đổi hành vi.
//
// Theo bộ quy tắc evon:ui-ux: phẳng (không gradient, không kính), viền 1px
// thay cho bóng, mỗi khu vực một nút đặc, chữ dài thì xuống dòng.

import {
  formatBytes,
  TRIGGER_LABEL,
  type GameCandidate,
  type GameScan,
  type LocalSnapshot,
  type RemoteSnapshot,
} from "./api";

export function esc(s: string): string {
  return s.replace(
    /[&<>"']/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!,
  );
}

// ── Icon (nét 1.8px, 24×24) ──────────────────────────────────────────────

const PATHS = {
  check: '<path d="M5 12.5l4.5 4.5L19 7.5"/>',
  play: '<path d="M8 5.5v13l10.5-6.5z" fill="currentColor"/>',
  stop: '<rect x="7" y="7" width="10" height="10" rx="1.5" fill="currentColor"/>',
  restore: '<path d="M4 12a8 8 0 1 0 2.4-5.7L4 8.5"/><path d="M4 4v4.5h4.5"/>',
  scan: '<path d="M20 11a8 8 0 0 0-14.3-4.9L4 8"/><path d="M4 4v4h4"/><path d="M4 13a8 8 0 0 0 14.3 4.9L20 16"/><path d="M20 20v-4h-4"/>',
  cloud: '<path d="M7 18h10a4 4 0 0 0 .6-7.96A6 6 0 0 0 6.1 9.1 4.5 4.5 0 0 0 7 18z"/>',
  upload: '<path d="M7 18h10a4 4 0 0 0 .6-7.96A6 6 0 0 0 6.1 9.1 4.5 4.5 0 0 0 7 18z"/><path d="M12 15v-5M9.8 12.2L12 10l2.2 2.2"/>',
  x: '<path d="M7 7l10 10M17 7L7 17"/>',
  warn: '<path d="M12 4l9 16H3z"/><path d="M12 10v4M12 17v.5"/>',
  dot: '<circle cx="12" cy="12" r="2.5" fill="currentColor"/>',
  user: '<circle cx="12" cy="9" r="3.5"/><path d="M5 19.5c1.2-3.2 3.9-5 7-5s5.8 1.8 7 5"/>',
  update: '<path d="M12 4v11M7.5 10.5L12 15l4.5-4.5"/><path d="M5 19.5h14"/>',
  save: '<path d="M5 4h11l3 3v13H5z"/><path d="M8 4v5h7V4M8 20v-6h8v6"/>',
  folder: '<path d="M3.5 6.5a2 2 0 0 1 2-2h4l2 2.5h7a2 2 0 0 1 2 2v8.5a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z"/>',
  chevron: '<path d="M9.5 6l6 6-6 6"/>',
  disk: '<rect x="3" y="4.5" width="18" height="15" rx="2.5"/><path d="M3 9.5h18"/>',
} as const;

export type IconName = keyof typeof PATHS;

export function icon(name: IconName, cls = "ic"): string {
  return `<svg class="${cls}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${PATHS[name]}</svg>`;
}

// ── Ô chữ viết tắt của game ──────────────────────────────────────────────

const STOP = new Set(["the", "of", "a", "an", "and", "to", "for", "in", "on"]);

/// "Stardew Valley" → SV, "Counter-Strike 2" → CS2, "DAVE THE DIVER" → DD,
/// "WorldBox - God Simulator" → WB, "Grounded 2" → G2.
export function initials(title: string): string {
  const main = title.split(/\s[-–—:|]\s|:\s/)[0] ?? title;
  const words = main
    .replace(/[™®©]/g, "")
    .split(/[\s\-_/]+/)
    .filter((w) => w && !STOP.has(w.toLowerCase()));
  const num = words.find((w) => /^\d+$/.test(w)) ?? "";
  const letters = words.filter((w) => !/^\d+$/.test(w));
  let out = "";
  if (letters.length === 1 && num) {
    out = letters[0].charAt(0);
  } else if (letters.length === 1) {
    // Một từ: lấy chữ hoa bên trong ("WorldBox" → WB), không thì 2 chữ đầu.
    const caps = letters[0].match(/[A-ZÀ-Ỹ]/g);
    out = caps && caps.length >= 2 ? caps.slice(0, 2).join("") : letters[0].slice(0, 2);
  } else {
    out = letters
      .slice(0, 2)
      .map((w) => w.charAt(0))
      .join("");
  }
  return (out + num).toUpperCase().slice(0, 3) || "?";
}

/// Xếp theo (mã chữ cái % 7) để chữ cái phổ biến rải đều màu: S → lime,
/// C → cam, D → xanh dương, G → xanh lá, W → tím.
const TONES = ["pink", "green", "cyan", "purple", "orange", "blue", "lime"] as const;

/// Màu ô theo chữ cái đầu của viết tắt — cố định cho mỗi game, không đổi khi
/// danh sách đổi thứ tự. Game chưa có bản lưu nào thì xám.
export function tileTone(title: string, hasSaves: boolean): string {
  if (!hasSaves) return "gray";
  const code = initials(title).charCodeAt(0) || 0;
  return TONES[code % TONES.length];
}

function tile(g: GameCandidate, hasSaves: boolean, size = ""): string {
  return `<span class="ftile ftile--${tileTone(g.title, hasSaves)}${size ? ` ftile--${size}` : ""}" aria-hidden="true">${esc(initials(g.title))}</span>`;
}

// ── Thời gian ────────────────────────────────────────────────────────────

const pad = (n: number) => String(n).padStart(2, "0");

export function hhmm(iso: string | Date): string {
  const d = typeof iso === "string" ? new Date(iso) : iso;
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function ddmm(iso: string): string {
  const d = new Date(iso);
  return `${pad(d.getDate())}/${pad(d.getMonth() + 1)}`;
}

function ddmmyyyy(iso: string): string {
  const d = new Date(iso);
  return `${ddmm(iso)}/${d.getFullYear()}`;
}

/// "4 ngày trước" → "4 ngày" cho gọn trong danh sách.
function ago(iso: string): string {
  const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
  if (s < 60) return "vừa xong";
  if (s < 3600) return `${Math.floor(s / 60)} phút`;
  if (s < 86400) return `${Math.floor(s / 3600)} giờ`;
  return `${Math.floor(s / 86400)} ngày`;
}

// ── Danh sách game ───────────────────────────────────────────────────────

export function gameItem(g: GameCandidate, onCloud: number, active: boolean): string {
  const hasSaves = g.local_count > 0 || onCloud > 0;
  const bits: string[] = [];
  if (g.running) bits.push(`<span class="flive"><span class="flive__dot"></span>Live</span>`);
  if (g.local_count > 0) bits.push(`${g.local_count} bản`);
  if (onCloud > 0) bits.push(`<span class="fcloud-n" title="${onCloud} bản trên cloud">${icon("cloud", "ic ic--xs")}${onCloud}</span>`);
  if (g.last_local_at) bits.push(ago(g.last_local_at));
  if (!hasSaves) bits.push("chưa lưu");
  if (!g.installed) bits.push("chưa cài");
  const meta = bits
    .map((b, i) => (i === 0 || bits[i - 1].startsWith('<span class="flive"') ? b : `· ${b}`))
    .join(" ");
  return `<li class="fgame${active ? " is-active" : ""}" data-app="${g.app_id}" tabindex="0" role="button" aria-pressed="${active}">
    ${tile(g, hasSaves)}
    <span class="fgame__body">
      <span class="fgame__title">${esc(g.title)}</span>
      <span class="fgame__meta">${meta}</span>
    </span>
  </li>`;
}

// ── Chi tiết game ────────────────────────────────────────────────────────

export interface DetailData {
  g: GameCandidate;
  scan: GameScan | null;
  locals: LocalSnapshot[];
  remotes: RemoteSnapshot[];
  session: boolean;
  cloudConfigured: boolean;
  showFiles: boolean;
}

const SOURCE: Record<string, string> = {
  ufs: "appinfo.vdf",
  remote_cache: "remotecache.vdf",
  manifest: "ludusavi",
};

export function detail(d: DetailData): string {
  const { g, scan, locals, remotes } = d;
  const hasSaves = locals.length > 0 || remotes.length > 0;
  const latest = locals[0];

  const tags = [
    g.running ? `<span class="fbadge fbadge--live"><span class="flive__dot"></span>Đang chạy</span>` : "",
    `<span class="fmono fmuted">APPID ${g.app_id}</span>`,
    !g.installed ? `<span class="fbadge">Chưa cài trên máy này</span>` : "",
  ].join("");

  const stat = (label: string, value: string, sub = "") =>
    `<div class="fstat"><dt>${label}</dt><dd><span class="fstat__v">${value}</span>${sub ? `<span class="fstat__s">${sub}</span>` : ""}</dd></div>`;
  const cloudStat = !d.cloudConfigured
    ? stat("Cloud", "—", "chưa bật")
    : d.session
      ? stat("Cloud", pad(remotes.length), "bản")
      : stat("Cloud", "—", "chưa đăng nhập");

  const head = `<section class="fcard fhead">
    ${tile(g, hasSaves, "xl")}
    <div class="fhead__main">
      <div class="fhead__tags">${tags}</div>
      <h2 class="fhead__title">${esc(g.title)}</h2>
    </div>
    <button id="btn-capture" class="ghost fhead__btn" data-guard ${g.running ? "disabled" : ""}
      title="${g.running ? "Game đang chạy — sẽ tự lưu khi tắt" : "Lưu trạng thái save hiện tại vào máy"}">
      ${icon("save")}Lưu ngay vào máy
    </button>
    <dl class="fstats">
      ${stat("Bản lưu", pad(locals.length))}
      ${latest ? stat("Mới nhất", hhmm(latest.created_at), ddmm(latest.created_at)) : stat("Mới nhất", "—")}
      ${scan ? stat("Trên đĩa", formatBytes(scan.total_bytes), `${scan.files.length} file`) : stat("Trên đĩa", "…", "đang quét")}
      ${cloudStat}
    </dl>
  </section>`;

  const lockNote = g.running ? `<span class="fnote">Khôi phục bị khoá khi game đang chạy</span>` : "";
  const localRows = locals.length
    ? locals
        .map((s, i) => {
          const idx = pad(locals.length - i);
          const tagsHtml = [
            i === 0 ? `<span class="ftag ftag--lime">Mới nhất</span>` : "",
            `<span class="ftag">${TRIGGER_LABEL[s.trigger]}</span>`,
            s.remote_id ? `<span class="ftag ftag--cyan">${icon("cloud", "ic ic--xs")}Đã lên cloud</span>` : "",
            s.warnings.length
              ? `<span class="ftag ftag--warn" title="${esc(s.warnings.join("\n"))}">${s.warnings.length} cảnh báo</span>`
              : "",
          ].join("");
          return `<div class="fsnap${i === 0 ? " is-latest" : ""}">
            <span class="fsnap__idx">#${idx}</span>
            <span class="fsnap__when"><strong>${hhmm(s.created_at)}</strong><span>${ddmmyyyy(s.created_at)}</span></span>
            <span class="fsnap__tags">${tagsHtml}</span>
            <span class="fsnap__meta">${s.files.length} file · ${formatBytes(s.total_bytes)}</span>
            <span class="fsnap__act">
              <button class="ghost ghost--accent" data-guard data-push="${s.id}" ${!d.cloudConfigured ? "disabled title='Bản này chưa bật cloud'" : ""}>
                ${icon("upload")}${s.remote_id ? "Đẩy lại" : "Đẩy lên cloud"}
              </button>
              <button class="ghost" data-guard data-restore-local="${s.id}" ${g.running ? "disabled" : ""}>
                ${icon("restore")}Khôi phục
              </button>
            </span>
          </div>`;
        })
        .join("")
    : `<p class="fempty">Chưa có bản lưu nào trên máy. Bấm “Lưu ngay vào máy” hoặc chơi rồi tắt game — app tự lưu.</p>`;

  const localCard = `<section class="fcard">
    <header class="fcard__head">
      <h3>${icon("disk")}Bản lưu trên máy <span class="fcount">[${pad(locals.length)}]</span></h3>
      ${lockNote}
    </header>
    <div class="fsnaps">${localRows}</div>
  </section>`;

  // Cloud: chưa đăng nhập → thẻ nhỏ mời đăng nhập; đã đăng nhập → danh sách.
  let cloudCard: string;
  if (!d.cloudConfigured) {
    cloudCard = `<section class="fcard fmini">
      <span class="fmini__ic">${icon("cloud")}</span>
      <div class="fmini__body"><h3>Bản trên cloud</h3><p>Bản này chưa bật cloud.</p></div>
    </section>`;
  } else if (!d.session) {
    cloudCard = `<section class="fcard fmini">
      <span class="fmini__ic fmini__ic--cyan">${icon("cloud")}</span>
      <div class="fmini__body"><h3>Bản trên cloud</h3><p>Đăng nhập để xem các bản đã đẩy lên.</p></div>
      <button class="primary primary--cyan" data-guard data-login>Đăng nhập</button>
    </section>`;
  } else {
    const rows = remotes.length
      ? remotes
          .map(
            (r, i) => `<div class="fsnap">
            <span class="fsnap__idx">#${pad(remotes.length - i)}</span>
            <span class="fsnap__when"><strong>${hhmm(r.created_at)}</strong><span>${ddmmyyyy(r.created_at)}</span></span>
            <span class="fsnap__tags"><span class="ftag">${esc(r.device_name ?? r.device_id)}</span></span>
            <span class="fsnap__meta">${r.file_count} file · ${formatBytes(r.total_bytes)}</span>
            <span class="fsnap__act">
              <button class="ghost" data-guard data-restore-remote="${r.id}" ${g.running ? "disabled" : ""}>${icon("restore")}Khôi phục</button>
              <button class="ghost danger" data-guard data-delete-remote="${r.id}">${icon("x")}Xoá</button>
            </span>
          </div>`,
          )
          .join("")
      : `<p class="fempty">Chưa có bản nào trên cloud. Bấm “Đẩy lên cloud” ở một bản lưu trên máy.</p>`;
    cloudCard = `<section class="fcard">
      <header class="fcard__head">
        <h3>${icon("cloud")}Bản trên cloud <span class="fcount">[${pad(remotes.length)}]</span></h3>
      </header>
      <div class="fsnaps">${rows}</div>
    </section>`;
  }

  const diskSummary = scan ? `${scan.files.length} file · ${formatBytes(scan.total_bytes)}` : "Đang quét…";
  const diskCard = `<section class="fcard fmini">
    <span class="fmini__ic">${icon("folder")}</span>
    <div class="fmini__body"><h3>File save trên đĩa</h3><p class="fmono">${diskSummary}</p></div>
    <button class="ghost" data-toggle-files aria-expanded="${d.showFiles}" ${scan ? "" : "disabled"}>
      ${d.showFiles ? "Ẩn file" : "Xem file"}${icon("chevron", `ic fchev${d.showFiles ? " is-open" : ""}`)}
    </button>
  </section>`;

  const files =
    d.showFiles && scan
      ? `<section class="fcard">
      <header class="fcard__head"><h3>${icon("folder")}File save trên đĩa <span class="fcount">[${pad(scan.files.length)}]</span></h3></header>
      ${
        scan.files.length
          ? `<div class="ftable-wrap"><table class="ftable">
              <thead><tr><th>Root</th><th>Đường dẫn</th><th class="num">Dung lượng</th><th>Nguồn</th></tr></thead>
              <tbody>${scan.files
                .map(
                  (f) => `<tr>
                  <td class="fmono">${esc(f.root)}</td>
                  <td class="ftable__path" title="${esc(f.abs_path)}">${esc(f.rel_path)}</td>
                  <td class="num fmono">${formatBytes(f.size)}</td>
                  <td><span class="ftag">${SOURCE[f.source] ?? f.source}</span></td>
                </tr>`,
                )
                .join("")}</tbody>
            </table></div>`
          : `<p class="fempty">Không có file nào.</p>`
      }
      ${scan.warnings.map((w) => `<p class="fwarn">${esc(w)}</p>`).join("")}
    </section>`
      : "";

  // Chưa đăng nhập: thẻ cloud và thẻ file nằm cạnh nhau (như thiết kế). Đã
  // đăng nhập: danh sách cloud chiếm cả hàng, thẻ file xuống dưới.
  const bottom = d.session
    ? `${cloudCard}${diskCard}`
    : `<div class="fpair">${cloudCard}${diskCard}</div>`;

  return `<div class="fdetail">${head}${localCard}${bottom}${files}</div>`;
}

// ── Nhật ký ──────────────────────────────────────────────────────────────

export type LogKind = "save" | "restore" | "session" | "scan" | "cloud" | "system";
export type LogTone = "info" | "ok" | "warn" | "error";

export interface LogEntry {
  at: Date;
  tone: LogTone;
  kind: LogKind;
  /// Câu đầy đủ (giao diện kính hiện nguyên câu này).
  text: string;
  /// HTML cho giao diện mặc định — tên game in đậm. Không có thì dùng `text`.
  html?: string;
  icon?: IconName;
  meta?: string;
  metaTone?: "lime" | "cyan" | "muted";
  /// Nhãn nhỏ bên phải (vd kết quả quét: "3 mới", "1 không đổi").
  chips?: { text: string; tone?: "lime" }[];
  dim?: boolean;
}

export type LogFilter = "all" | "save" | "restore" | "session";

function dayLabel(d: Date): string {
  const today = new Date();
  const y = new Date(today);
  y.setDate(today.getDate() - 1);
  const same = (a: Date, b: Date) =>
    a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
  if (same(d, today)) return "Hôm nay";
  if (same(d, y)) return "Hôm qua";
  return `${pad(d.getDate())}/${pad(d.getMonth() + 1)}/${d.getFullYear()}`;
}

function defaultIcon(e: LogEntry): IconName {
  if (e.icon) return e.icon;
  if (e.tone === "error") return "x";
  if (e.tone === "warn") return "warn";
  if (e.kind === "restore") return "restore";
  if (e.kind === "cloud") return "cloud";
  if (e.tone === "ok") return "check";
  return "dot";
}

/// Mục trong `entries` mới nhất trước.
export function logList(entries: LogEntry[], filter: LogFilter): string {
  // "Lưu" gồm cả đẩy lên cloud — cả hai đều là cất save đi.
  const shown = entries.filter(
    (e) => filter === "all" || e.kind === filter || (filter === "save" && e.kind === "cloud"),
  );
  if (!shown.length) {
    return `<li class="flog__empty">${
      entries.length ? "Không có mục nào khớp bộ lọc." : "Chưa có hoạt động nào. Chơi một game rồi tắt — app tự lưu và ghi lại ở đây."
    }</li>`;
  }
  let out = "";
  let day = "";
  for (const e of shown) {
    const label = dayLabel(e.at);
    if (label !== day) {
      day = label;
      out += `<li class="flog__day">${label}</li>`;
    }
    const ic = defaultIcon(e);
    const strong = e.kind === "save" && e.tone === "ok" && ic === "check";
    const right = e.chips?.length
      ? `<span class="flog__chips">${e.chips.map((c) => `<span class="ftag${c.tone ? ` ftag--${c.tone}` : ""}">${esc(c.text)}</span>`).join("")}</span>`
      : e.meta
        ? `<span class="flog__meta flog__meta--${e.metaTone ?? "muted"}">${esc(e.meta)}</span>`
        : "";
    out += `<li class="flog__row flog__row--${e.tone}${e.dim ? " is-dim" : ""}">
      <time class="flog__time">${hhmm(e.at)}</time>
      <span class="flog__ic flog__ic--${ic}${strong ? " is-strong" : ""} flog__ic--k-${e.kind}">${icon(ic)}</span>
      <span class="flog__text">${e.html ?? esc(e.text)}</span>
      ${right}
    </li>`;
  }
  return out;
}
