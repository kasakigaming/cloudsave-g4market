// Lớp bọc mỏng quanh các lệnh Tauri, kèm kiểu dữ liệu khớp với phía Rust.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type RootToken = string;
export type Source = "ufs" | "remote_cache" | "manifest";
export type Trigger = "scan_all" | "manual" | "game_exit";

export interface SteamUser {
  account_id: number;
  steam_id64: string;
  account_name: string | null;
  persona_name: string | null;
  last_login: number | null;
}

export interface SteamStatus {
  root: string;
  library_count: number;
  users: SteamUser[];
  appinfo_apps: number;
  active_account: number | null;
}

export interface SteamAccount {
  account_id: number;
  steam_id64: string;
  persona_name: string | null;
  account_name: string | null;
  logged_in: boolean;
  avatar: string | null;
}

export interface SteamCloudStatus {
  account_id: number | null;
  /** queued = còn bật, nhưng app đã ghi sẵn "tắt" cho lần đăng nhập tới. */
  state: "on" | "queued" | "off" | "unknown";
  steam_running: boolean;
  /** Tài khoản này đang đăng nhập Steam ngay lúc này. */
  logged_in: boolean;
  auto_disable: boolean;
  busy: boolean;
  error: string | null;
}

export interface BgImage {
  id: string;
  path: string;
}

export interface WebPhoto {
  url: string;
  author: string;
  license: string;
  page: string;
}

export interface AddBgReport {
  added: BgImage[];
  skipped: string[];
}

export interface GameCandidate {
  app_id: number;
  title: string;
  slug: string;
  installed: boolean;
  has_ufs: boolean;
  has_userdata: boolean;
  in_manifest: boolean;
  running: boolean;
  local_count: number;
  last_local_at: string | null;
  latest_pushed: boolean;
}

export interface SaveFile {
  root: RootToken;
  rel_path: string;
  abs_path: string;
  size: number;
  mtime: string | null;
  source: Source;
}

export interface GameScan {
  app_id: number | null;
  slug: string;
  title: string;
  files: SaveFile[];
  total_bytes: number;
  sources: Source[];
  warnings: string[];
}

export interface LocalFile {
  root: RootToken;
  rel_path: string;
  size: number;
  mtime: string | null;
  hash: string;
}

export interface LocalSnapshot {
  id: string;
  game_slug: string;
  game_title: string;
  steam_appid: number | null;
  account_id: number;
  created_at: string;
  trigger: Trigger;
  files: LocalFile[];
  total_bytes: number;
  warnings: string[];
  preview: unknown;
  remote_id: string | null;
  pushed_at: string | null;
}

export type CaptureOutcome =
  | { kind: "created"; snapshot: LocalSnapshot }
  | { kind: "unchanged"; snapshot: LocalSnapshot }
  | { kind: "empty" };

export interface RunningGame {
  app_id: number;
  title: string;
}

export interface GameChecked {
  app_id: number;
  title: string;
  trigger: Trigger;
  outcome: CaptureOutcome | null;
  error: string | null;
}

export interface ScanAllReport {
  total: number;
  created: number;
  unchanged: number;
  empty: number;
  skipped_running: number;
  failed: number;
  errors: string[];
}

export type ScanProgress =
  | { kind: "started"; total: number }
  | { kind: "game"; done: number; total: number; app_id: number; title: string; result: string }
  | { kind: "done" };

export interface Session {
  access_token: string;
  refresh_token: string;
  user_id: string;
  email: string | null;
}

export type SignUpOutcome =
  | { kind: "signed_in"; session: Session }
  | { kind: "needs_confirmation"; email: string }
  | { kind: "already_registered" };

export interface BackupReport {
  snapshot_id: string;
  local_id: string;
  file_count: number;
  total_bytes: number;
  uploaded_bytes: number;
  /// Số byte thật sự lưu trên cloud sau khi nén / delta.
  stored_bytes: number;
  deduped_bytes: number;
  delta_files: number;
}

export type PushProgress =
  | { kind: "started"; total_files: number; total_bytes: number }
  | { kind: "packing"; file: string }
  | { kind: "uploading"; file: string; stored_bytes: number; delta: boolean }
  | { kind: "finalizing" }
  | {
      kind: "done";
      snapshot_id: string;
      uploaded_bytes: number;
      stored_bytes: number;
      deduped_bytes: number;
    };

export interface RestoreReport {
  restored: number;
  skipped: number;
  safety_dir: string | null;
  warnings: string[];
  /// Tài khoản Steam đã nhận bản khôi phục (tài khoản đang đăng nhập).
  target_account: number;
  /// Tài khoản đã chụp bản lưu, nếu biết hoặc dò ra được.
  source_account: number | null;
  /// recorded = bản lưu có ghi; path / path_newest = dò từ đường dẫn; unknown.
  source_origin: "recorded" | "path" | "path_newest" | "unknown";
  /// Số file phải đổi id tài khoản trong đường dẫn.
  remapped: number;
  /// Số `steam_autocloud.vdf` phải dán lại nhãn cho tài khoản đích.
  markers: number;
}

export interface RemoteSnapshot {
  id: string;
  game_slug: string;
  game_title: string;
  steam_appid: number | null;
  device_name: string | null;
  device_id: string;
  file_count: number;
  total_bytes: number;
  status: string;
  created_at: string;
}

export const api = {
  // Steam
  detectSteam: () => invoke<SteamStatus>("detect_steam"),
  setAccount: (accountId: number) => invoke<void>("set_account", { accountId }),
  steamAccount: () => invoke<SteamAccount | null>("steam_account"),
  listBackgrounds: () => invoke<BgImage[]>("list_backgrounds"),
  pickBackgrounds: () => invoke<AddBgReport>("pick_backgrounds"),
  removeBackground: (id: string) => invoke<void>("remove_background", { id }),
  webBackgrounds: () => invoke<WebPhoto[]>("web_backgrounds"),
  steamCloudStatus: () => invoke<SteamCloudStatus>("steam_cloud_status"),
  setSteamCloud: (enabled: boolean) => invoke<SteamCloudStatus>("set_steam_cloud", { enabled }),
  setAutoDisableSteamCloud: (on: boolean) =>
    invoke<SteamCloudStatus>("set_auto_disable_steam_cloud", { on }),
  listGames: (accountId: number) => invoke<GameCandidate[]>("list_games", { accountId }),
  scanGame: (accountId: number, appId: number) =>
    invoke<GameScan>("scan_game", { accountId, appId }),
  runningGames: () => invoke<RunningGame[]>("running_games"),

  // Local
  scanAll: (accountId: number) => invoke<ScanAllReport>("scan_all", { accountId }),
  captureGame: (appId: number) => invoke<GameChecked>("capture_game", { appId }),
  listLocal: (gameSlug?: string) =>
    invoke<LocalSnapshot[]>("list_local", { gameSlug: gameSlug ?? null }),
  restoreLocal: (snapshotId: string) => invoke<RestoreReport>("restore_local", { snapshotId }),

  // Cloud
  supabaseConfigured: () => invoke<boolean>("supabase_configured"),
  signIn: (email: string, password: string) => invoke<Session>("sign_in", { email, password }),
  signUp: (email: string, password: string) =>
    invoke<SignUpOutcome>("sign_up", { email, password }),
  signOut: () => invoke<void>("sign_out"),
  currentSession: () => invoke<Session | null>("current_session"),
  pushLocal: (snapshotId: string, force: boolean) =>
    invoke<BackupReport>("push_local", { snapshotId, force }),
  listRemote: (gameSlug?: string) =>
    invoke<RemoteSnapshot[]>("list_snapshots", { gameSlug: gameSlug ?? null }),
  restoreRemote: (snapshotId: string, appId: number) =>
    invoke<RestoreReport>("restore_snapshot", { snapshotId, appId }),
  deleteRemote: (snapshotId: string) => invoke<number>("delete_snapshot", { snapshotId }),
  reconcileCloud: () => invoke<number>("reconcile_cloud"),

  refreshManifest: () => invoke<number>("refresh_manifest"),
  deviceInfo: () => invoke<{ id: string; name: string; store: string }>("device_info"),

  on: <T>(event: string, cb: (p: T) => void): Promise<UnlistenFn> =>
    listen<T>(event, (e) => cb(e.payload)),
};

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v < 10 ? 1 : 0)} ${units[i]}`;
}

export function formatDate(iso: string): string {
  return new Date(iso).toLocaleString("vi-VN", {
    day: "2-digit",
    month: "2-digit",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function timeAgo(iso: string): string {
  const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
  if (s < 60) return "vừa xong";
  if (s < 3600) return `${Math.floor(s / 60)} phút trước`;
  if (s < 86400) return `${Math.floor(s / 3600)} giờ trước`;
  return `${Math.floor(s / 86400)} ngày trước`;
}

export const TRIGGER_LABEL: Record<Trigger, string> = {
  scan_all: "Quét",
  manual: "Thủ công",
  game_exit: "Khi tắt game",
};

/// Dịch mã lỗi của Supabase Auth (GoTrue) sang câu người dùng hiểu được.
/// Lỗi từ Rust về dạng "Supabase trả lỗi 429: {...error_code...}".
export function authErrorText(e: unknown): string {
  const t = errorText(e);
  const map: [string, string][] = [
    [
      "over_email_send_rate_limit",
      "Supabase đã hết lượt gửi email xác nhận (máy chủ email mặc định chỉ gửi khoảng 2 thư/giờ). " +
        "Đợi khoảng 1 giờ, hoặc tắt \"Confirm email\" trong Supabase Dashboard để đăng ký không cần email.",
    ],
    ["over_request_rate_limit", "Thử quá nhiều lần — đợi vài phút rồi thử lại."],
    ["email_not_confirmed", "Email chưa xác nhận — mở hộp thư và bấm link xác nhận."],
    ["invalid_credentials", "Sai email hoặc mật khẩu."],
    ["user_already_exists", "Email này đã có tài khoản — hãy đăng nhập."],
    ["email_exists", "Email này đã có tài khoản — hãy đăng nhập."],
    ["weak_password", "Mật khẩu quá yếu — dùng ít nhất 6 ký tự, nên có chữ hoa, số và ký hiệu."],
    ["email_address_invalid", "Địa chỉ email không hợp lệ."],
    ["signup_disabled", "Project đang tắt đăng ký tài khoản mới."],
  ];
  for (const [code, msg] of map) if (t.includes(code)) return msg;
  if (t.includes("lỗi mạng")) return "Không kết nối được Supabase — kiểm tra mạng.";
  return t;
}

/// Lỗi từ Rust về dưới dạng chuỗi đã dịch sẵn; các lỗi khác thì hiện thô.
export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return String(e);
}
