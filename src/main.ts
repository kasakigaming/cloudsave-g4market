import {
  api,
  authErrorText,
  errorText,
  formatBytes,
  formatDate,
  timeAgo,
  TRIGGER_LABEL,
  type GameCandidate,
  type GameChecked,
  type GameScan,
  type LocalSnapshot,
  type PushProgress,
  type RemoteSnapshot,
  type RunningGame,
  type ScanProgress,
  type Session,
  type SteamUser,
} from "./api";
import "./styles.css";

// ── State ────────────────────────────────────────────────────────────────

let users: SteamUser[] = [];
let accountId: number | null = null;
let games: GameCandidate[] = [];
let filter = "";
let selectedAppId: number | null = null;
let diskScan: GameScan | null = null;
let locals: LocalSnapshot[] = [];
let remotes: RemoteSnapshot[] = [];
let session: Session | null = null;
let cloudConfigured = false;
let running: RunningGame[] = [];
let scanning = false;
let busy = false;

const $ = <T extends HTMLElement>(sel: string) => document.querySelector<T>(sel)!;

const selected = () => games.find((g) => g.app_id === selectedAppId) ?? null;

function setStatus(msg: string, tone: "info" | "error" | "ok" = "info") {
  const el = $("#status");
  el.textContent = msg;
  el.className = `status status--${tone}`;
}

function setBusy(v: boolean) {
  busy = v;
  document.querySelectorAll<HTMLButtonElement>("button[data-guard]").forEach((b) => {
    b.disabled = v;
  });
}

/// Nhật ký hoạt động ở cột phải — nơi người dùng thấy watcher đã làm gì
/// trong lúc họ đang chơi.
function log(msg: string, tone: "info" | "ok" | "warn" | "error" = "info") {
  const li = document.createElement("li");
  li.className = `log__item log__item--${tone}`;
  const time = new Date().toLocaleTimeString("vi-VN", { hour: "2-digit", minute: "2-digit" });
  li.innerHTML = `<time>${time}</time><span>${escapeHtml(msg)}</span>`;
  const list = $("#log");
  list.prepend(li);
  while (list.children.length > 200) list.lastElementChild?.remove();
}

// ── Khởi động ────────────────────────────────────────────────────────────

async function boot() {
  wireEvents();
  await subscribe();

  const device = await api.deviceInfo();
  $("#device-name").textContent = device.name;

  cloudConfigured = await api.supabaseConfigured();
  session = cloudConfigured ? await api.currentSession() : null;
  renderCloud();

  try {
    const steam = await api.detectSteam();
    users = steam.users;
    $("#steam-meta").textContent =
      `${steam.library_count} thư viện · ${steam.appinfo_apps.toLocaleString("vi-VN")} app`;
    accountId = steam.active_account ?? users[0]?.account_id ?? null;
    renderUsers();
  } catch (e) {
    setStatus(`Không đọc được Steam: ${errorText(e)}`, "error");
    log(`Không đọc được Steam: ${errorText(e)}`, "error");
    return;
  }

  running = await api.runningGames();
  renderPlaying();

  if (accountId !== null) {
    await refreshGames();
    // Bước đầu tiên: kiểm tra hết save đang có và lưu vào máy.
    await runScanAll();
  }
}

function wireEvents() {
  $("#user-select").addEventListener("change", (e) => {
    void selectUser(Number((e.target as HTMLSelectElement).value));
  });
  $("#game-filter").addEventListener("input", (e) => {
    filter = (e.target as HTMLInputElement).value.toLowerCase();
    renderGames();
  });
  $("#btn-scan-all").addEventListener("click", () => void runScanAll());
  $("#btn-refresh-manifest").addEventListener("click", () => void doRefreshManifest());
  $("#btn-cloud-login").addEventListener("click", () => openLogin());
  $("#btn-cloud-logout").addEventListener("click", () => void doSignOut());

  $("#login-form").addEventListener("submit", (e) => {
    e.preventDefault();
    void doSignIn();
  });
  $("#btn-signup").addEventListener("click", () => void doSignUp());
  $("#btn-login-cancel").addEventListener("click", () => closeLogin());
}

async function subscribe() {
  await api.on<RunningGame[]>("running-games", (list) => {
    running = list;
    renderPlaying();
    for (const g of games) g.running = list.some((r) => r.app_id === g.app_id);
    renderGames();
    if (selected()) renderDetail();
  });
  await api.on<RunningGame>("game-started", (g) => log(`▶ Bắt đầu chơi ${g.title}`, "info"));
  await api.on<RunningGame>("game-exited", (g) =>
    log(`■ ${g.title} đã tắt — đang kiểm tra file save…`, "info"),
  );
  await api.on<GameChecked>("game-checked", (c) => void onGameChecked(c));
  await api.on<ScanProgress>("scan-progress", onScanProgress);
  await api.on<PushProgress>("push-progress", onPushProgress);
}

// ── Watcher ──────────────────────────────────────────────────────────────

function renderPlaying() {
  const el = $("#now-playing");
  const text = $("#now-playing-text");
  if (running.length === 0) {
    el.className = "playing playing--idle";
    text.textContent = "Không có game nào đang chạy";
  } else {
    el.className = "playing playing--live";
    text.textContent = `Đang chơi: ${running.map((r) => r.title).join(", ")}`;
  }
}

async function onGameChecked(c: GameChecked) {
  const who = c.title;
  if (c.error) {
    log(`✗ ${who}: ${c.error}`, "error");
  } else if (c.outcome?.kind === "created") {
    const s = c.outcome.snapshot;
    log(
      `✓ ${who}: đã lưu bản mới vào máy (${s.files.length} file, ${formatBytes(s.total_bytes)})`,
      "ok",
    );
  } else if (c.outcome?.kind === "unchanged") {
    log(`= ${who}: save không đổi, không cần lưu thêm`, "info");
  } else {
    log(`○ ${who}: không tìm thấy file save`, "warn");
  }
  await refreshGames();
  if (selectedAppId === c.app_id) await loadDetail();
}

// ── Tài khoản Steam ──────────────────────────────────────────────────────

function renderUsers() {
  const sel = $<HTMLSelectElement>("#user-select");
  // Tài khoản đăng nhập gần nhất lên đầu — máy có thể có hàng chục tài khoản.
  const sorted = [...users].sort((a, b) => (b.last_login ?? 0) - (a.last_login ?? 0));
  sel.innerHTML = sorted
    .map((u) => {
      const name = u.persona_name || u.account_name || `Tài khoản ${u.account_id}`;
      const mark = u.account_id === accountId ? " selected" : "";
      return `<option value="${u.account_id}"${mark}>${escapeHtml(name)} (${u.account_id})</option>`;
    })
    .join("");
  sel.disabled = users.length === 0;
}

async function selectUser(id: number) {
  accountId = id;
  selectedAppId = null;
  await api.setAccount(id);
  renderDetail();
  await refreshGames();
  await runScanAll();
}

// ── Danh sách game ───────────────────────────────────────────────────────

async function refreshGames() {
  if (accountId === null) return;
  try {
    games = await api.listGames(accountId);
    renderGames();
  } catch (e) {
    setStatus(`Không liệt kê được game: ${errorText(e)}`, "error");
  }
}

function renderGames() {
  const list = $("#game-list");
  const visible = games.filter((g) => g.title.toLowerCase().includes(filter));
  if (visible.length === 0) {
    list.innerHTML = `<li class="empty">${games.length ? "Không có game nào khớp." : "Chưa có game nào có save."}</li>`;
    return;
  }

  // Game đang chạy lên đầu, rồi tới game có bản lưu.
  visible.sort(
    (a, b) =>
      Number(b.running) - Number(a.running) ||
      Number(b.local_count > 0) - Number(a.local_count > 0) ||
      a.title.localeCompare(b.title),
  );

  list.innerHTML = visible
    .map((g) => {
      const tags = [
        g.running ? `<span class="tag tag--live">● đang chạy</span>` : "",
        g.local_count > 0
          ? `<span class="tag tag--local" title="Bản lưu trên máy">${g.local_count} bản</span>`
          : `<span class="tag tag--muted">chưa lưu</span>`,
        g.latest_pushed ? `<span class="tag tag--cloud" title="Bản mới nhất đã lên cloud">☁ cloud</span>` : "",
      ].join("");
      const when = g.last_local_at ? `<span class="game__when">${timeAgo(g.last_local_at)}</span>` : "";
      const active = g.app_id === selectedAppId ? " is-active" : "";
      return `<li class="game${active}" data-app="${g.app_id}">
        <span class="game__title">${escapeHtml(g.title)}</span>
        <span class="game__meta">${tags}${when}</span>
      </li>`;
    })
    .join("");

  list.querySelectorAll<HTMLLIElement>(".game").forEach((li) => {
    li.addEventListener("click", () => {
      selectedAppId = Number(li.dataset.app);
      renderGames();
      void loadDetail();
    });
  });
}

// ── Quét tất cả ──────────────────────────────────────────────────────────

async function runScanAll() {
  if (accountId === null || scanning) return;
  scanning = true;
  setBusy(true);
  log("Bắt đầu quét toàn bộ save…");
  try {
    const r = await api.scanAll(accountId);
    const parts = [
      `${r.created} bản mới`,
      `${r.unchanged} không đổi`,
      r.empty ? `${r.empty} không có save` : "",
      r.skipped_running ? `${r.skipped_running} đang chạy` : "",
      r.failed ? `${r.failed} lỗi` : "",
    ].filter(Boolean);
    const msg = `Quét xong ${r.total} game: ${parts.join(", ")}.`;
    setStatus(msg, r.failed ? "error" : "ok");
    log(msg, r.failed ? "warn" : "ok");
    for (const e of r.errors) log(e, "error");
    await refreshGames();
    if (selectedAppId !== null) await loadDetail();
  } catch (e) {
    setStatus(`Quét thất bại: ${errorText(e)}`, "error");
    log(`Quét thất bại: ${errorText(e)}`, "error");
  } finally {
    $("#scan-progress").textContent = "";
    scanning = false;
    setBusy(false);
  }
}

function onScanProgress(p: ScanProgress) {
  const el = $("#scan-progress");
  if (p.kind === "started") el.textContent = `0/${p.total}`;
  else if (p.kind === "game") {
    el.textContent = `${p.done}/${p.total} · ${p.title}`;
    if (p.result.startsWith("bản mới")) log(`✓ ${p.title}: ${p.result}`, "ok");
    else if (p.result.startsWith("lỗi")) log(`✗ ${p.title}: ${p.result}`, "error");
  } else el.textContent = "";
}

// ── Chi tiết game ────────────────────────────────────────────────────────

async function loadDetail() {
  const g = selected();
  if (!g || accountId === null) return;
  diskScan = null;
  renderDetail();
  try {
    const [scan, loc] = await Promise.all([
      api.scanGame(accountId, g.app_id),
      api.listLocal(g.slug),
    ]);
    diskScan = scan;
    locals = loc;
    remotes = session ? await api.listRemote(g.slug).catch(() => []) : [];
    renderDetail();
  } catch (e) {
    setStatus(`Không tải được chi tiết: ${errorText(e)}`, "error");
  }
}

const SOURCE_LABEL: Record<string, string> = {
  ufs: "appinfo.vdf",
  remote_cache: "remotecache.vdf",
  manifest: "ludusavi",
};

function renderDetail() {
  const el = $("#detail");
  const g = selected();
  if (!g) {
    el.innerHTML = `<p class="empty">Chọn một game ở cột bên trái.</p>`;
    return;
  }

  const head = `
    <div class="detail__head">
      <div>
        <h2>${escapeHtml(g.title)}</h2>
        <p class="sub">appid ${g.app_id} · ${g.running ? `<strong class="live">đang chạy</strong>` : "không chạy"}</p>
      </div>
      <button id="btn-capture" data-guard ${g.running ? "disabled" : ""}
        title="${g.running ? "Game đang chạy — sẽ tự lưu khi tắt" : "Lưu trạng thái save hiện tại vào máy"}">
        Lưu ngay vào máy
      </button>
    </div>`;

  if (!diskScan) {
    el.innerHTML = `${head}<p class="empty">Đang quét…</p>`;
    wireDetail();
    return;
  }

  const files = diskScan.files
    .map(
      (f) => `<tr>
        <td><code>${escapeHtml(f.root)}</code></td>
        <td class="path" title="${escapeHtml(f.abs_path)}">${escapeHtml(f.rel_path)}</td>
        <td class="num">${formatBytes(f.size)}</td>
        <td><span class="src src--${f.source}">${SOURCE_LABEL[f.source] ?? f.source}</span></td>
      </tr>`,
    )
    .join("");

  const localRows = locals.length
    ? locals
        .map(
          (s, i) => `<div class="snap">
          <div class="snap__main">
            <strong>${formatDate(s.created_at)}</strong>
            ${i === 0 ? `<span class="tag tag--head">mới nhất</span>` : ""}
            <span class="tag tag--muted">${TRIGGER_LABEL[s.trigger]}</span>
            ${s.remote_id ? `<span class="tag tag--cloud">☁ đã lên cloud</span>` : ""}
            <div class="snap__meta">${s.files.length} file · ${formatBytes(s.total_bytes)}
              ${s.warnings.length ? ` · <span class="warn-text" title="${escapeHtml(s.warnings.join("\n"))}">${s.warnings.length} cảnh báo</span>` : ""}
            </div>
          </div>
          <div class="snap__actions">
            <button data-guard data-push="${s.id}" ${!cloudConfigured ? "disabled title='Chưa cấu hình Supabase'" : ""}>
              ${s.remote_id ? "Đẩy lại" : "Đẩy lên cloud"}
            </button>
            <button data-guard data-restore-local="${s.id}" ${g.running ? "disabled" : ""}>Khôi phục</button>
          </div>
        </div>`,
        )
        .join("")
    : `<p class="empty">Chưa có bản lưu nào trên máy.</p>`;

  const remoteBlock = !session
    ? `<p class="empty">Đăng nhập cloud để xem bản đã đẩy lên Supabase.</p>`
    : remotes.length
      ? remotes
          .map(
            (r) => `<div class="snap">
            <div class="snap__main">
              <strong>${formatDate(r.created_at)}</strong>
              <div class="snap__meta">${r.file_count} file · ${formatBytes(r.total_bytes)} · từ ${escapeHtml(r.device_name ?? r.device_id)}</div>
            </div>
            <div class="snap__actions">
              <button data-guard data-restore-remote="${r.id}" ${g.running ? "disabled" : ""}>Khôi phục</button>
              <button data-guard data-delete-remote="${r.id}" class="danger">Xoá</button>
            </div>
          </div>`,
          )
          .join("")
      : `<p class="empty">Chưa có bản nào trên cloud.</p>`;

  el.innerHTML = `
    ${head}
    <h3 class="section">Bản lưu trên máy <span class="muted">(${locals.length})</span></h3>
    ${localRows}

    <h3 class="section">Bản trên cloud</h3>
    ${remoteBlock}

    <details class="disk">
      <summary>File save hiện tại trên đĩa — ${diskScan.files.length} file, ${formatBytes(diskScan.total_bytes)}</summary>
      ${
        diskScan.files.length
          ? `<table class="files">
              <thead><tr><th>Root</th><th>Đường dẫn</th><th class="num">Dung lượng</th><th>Nguồn</th></tr></thead>
              <tbody>${files}</tbody>
            </table>`
          : `<p class="empty">Không có file nào.</p>`
      }
      ${diskScan.warnings.map((w) => `<p class="warn-text">${escapeHtml(w)}</p>`).join("")}
    </details>
  `;
  wireDetail();
}

function wireDetail() {
  const el = $("#detail");
  // Vẽ lại giữa lúc đang bận (vd watcher báo game vừa tắt) không được bật lại
  // các nút đang bị khoá.
  if (busy) el.querySelectorAll<HTMLButtonElement>("button[data-guard]").forEach((b) => (b.disabled = true));
  el.querySelector<HTMLButtonElement>("#btn-capture")?.addEventListener("click", () => void doCapture());
  el.querySelectorAll<HTMLButtonElement>("[data-push]").forEach((b) =>
    b.addEventListener("click", () => void doPush(b.dataset.push!, false)),
  );
  el.querySelectorAll<HTMLButtonElement>("[data-restore-local]").forEach((b) =>
    b.addEventListener("click", () => void doRestoreLocal(b.dataset.restoreLocal!)),
  );
  el.querySelectorAll<HTMLButtonElement>("[data-restore-remote]").forEach((b) =>
    b.addEventListener("click", () => void doRestoreRemote(b.dataset.restoreRemote!)),
  );
  el.querySelectorAll<HTMLButtonElement>("[data-delete-remote]").forEach((b) =>
    b.addEventListener("click", () => void doDeleteRemote(b.dataset.deleteRemote!)),
  );
}

// ── Hành động local ──────────────────────────────────────────────────────

async function doCapture() {
  const g = selected();
  if (!g) return;
  setBusy(true);
  try {
    await api.captureGame(g.app_id); // kết quả về qua sự kiện game-checked
  } catch (e) {
    setStatus(errorText(e), "error");
  } finally {
    setBusy(false);
  }
}

async function doRestoreLocal(id: string) {
  const ok = window.confirm(
    "Khôi phục bản này sẽ ghi đè file save hiện tại.\n\n" +
      "File đang có được cất vào thư mục cứu hộ trước khi ghi đè.",
  );
  if (!ok) return;
  setBusy(true);
  try {
    const r = await api.restoreLocal(id);
    const where = r.safety_dir ? ` Bản cũ ở: ${r.safety_dir}` : "";
    setStatus(`Đã khôi phục ${r.restored} file từ máy.${where}`, "ok");
    log(`↺ Khôi phục từ máy: ${r.restored} file${r.skipped ? `, bỏ qua ${r.skipped}` : ""}`, "ok");
    for (const w of r.warnings) log(w, "warn");
    await loadDetail();
  } catch (e) {
    setStatus(`Khôi phục thất bại: ${errorText(e)}`, "error");
  } finally {
    setBusy(false);
  }
}

// ── Hành động cloud ──────────────────────────────────────────────────────

async function doPush(id: string, force: boolean) {
  if (!session) {
    openLogin(() => void doPush(id, force));
    return;
  }
  setBusy(true);
  try {
    const r = await api.pushLocal(id, force);
    const parts = [`${r.file_count} file`];
    if (r.uploaded_bytes > 0) {
      const ratio = ((100 * r.stored_bytes) / r.uploaded_bytes).toFixed(1);
      parts.push(`${formatBytes(r.uploaded_bytes)} nén còn ${formatBytes(r.stored_bytes)} (${ratio}%)`);
    }
    if (r.delta_files) parts.push(`${r.delta_files} file chỉ lưu phần thay đổi`);
    if (r.deduped_bytes) parts.push(`bỏ qua ${formatBytes(r.deduped_bytes)} đã có sẵn`);
    const msg = `Đã đẩy lên cloud: ${parts.join(", ")}.`;
    setStatus(msg, "ok");
    log(`☁ ${msg}`, "ok");
    await refreshGames();
    await loadDetail();
  } catch (e) {
    const msg = errorText(e);
    if (msg.includes("xung đột")) {
      setBusy(false);
      const ok = window.confirm(
        `${msg}\n\nVẫn đẩy bản của máy này lên? Bản của máy kia vẫn được giữ trong lịch sử cloud.`,
      );
      if (ok) return void doPush(id, true);
      setStatus("Đã huỷ đẩy lên cloud.");
    } else if (msg.includes("chưa đăng nhập")) {
      session = null;
      renderCloud();
      openLogin(() => void doPush(id, force));
    } else {
      setStatus(`Đẩy thất bại: ${msg}`, "error");
      log(`✗ Đẩy thất bại: ${msg}`, "error");
    }
  } finally {
    setBusy(false);
  }
}

function onPushProgress(p: PushProgress) {
  if (p.kind === "packing") setStatus(`Đang nén ${p.file}…`);
  else if (p.kind === "uploading")
    setStatus(`Tải lên ${p.file} — ${formatBytes(p.stored_bytes)}${p.delta ? " (chỉ phần thay đổi)" : ""}`);
  else if (p.kind === "finalizing") setStatus("Đang ghi metadata…");
}

async function doRestoreRemote(id: string) {
  const g = selected();
  if (!g) return;
  const ok = window.confirm(
    "Khôi phục bản trên cloud sẽ ghi đè file save hiện tại.\n\n" +
      "File đang có được cất vào thư mục cứu hộ trước khi ghi đè.",
  );
  if (!ok) return;
  setBusy(true);
  try {
    const r = await api.restoreRemote(id, g.app_id);
    setStatus(`Đã khôi phục ${r.restored} file từ cloud.`, "ok");
    log(`↺ Khôi phục từ cloud: ${r.restored} file`, "ok");
    await loadDetail();
  } catch (e) {
    setStatus(`Khôi phục thất bại: ${errorText(e)}`, "error");
  } finally {
    setBusy(false);
  }
}

async function doDeleteRemote(id: string) {
  if (!window.confirm("Xoá vĩnh viễn bản này khỏi cloud? Bản trên máy không bị ảnh hưởng.")) return;
  setBusy(true);
  try {
    await api.deleteRemote(id);
    log("Đã xoá một bản trên cloud", "info");
    await refreshGames();
    await loadDetail();
  } catch (e) {
    setStatus(`Xoá thất bại: ${errorText(e)}`, "error");
  } finally {
    setBusy(false);
  }
}

// ── Đăng nhập cloud ──────────────────────────────────────────────────────

let afterLogin: (() => void) | null = null;

function renderCloud() {
  const status = $("#cloud-status");
  const login = $<HTMLButtonElement>("#btn-cloud-login");
  const logout = $<HTMLButtonElement>("#btn-cloud-logout");
  if (!cloudConfigured) {
    status.textContent = "Cloud: chưa cấu hình";
    login.hidden = true;
    logout.hidden = true;
  } else if (session) {
    status.textContent = `☁ ${session.email ?? session.user_id}`;
    login.hidden = true;
    logout.hidden = false;
  } else {
    status.textContent = "Cloud: chưa đăng nhập";
    login.hidden = false;
    logout.hidden = true;
  }
}

function openLogin(then?: () => void) {
  if (!cloudConfigured) {
    setStatus("Chưa cấu hình Supabase (thiếu SUPABASE_URL / SUPABASE_ANON_KEY trong .env).", "error");
    return;
  }
  afterLogin = then ?? null;
  $("#login-msg").textContent = "";
  $<HTMLDialogElement>("#login-dialog").showModal();
  $<HTMLInputElement>("#login-email").focus();
}

function closeLogin() {
  afterLogin = null;
  $<HTMLDialogElement>("#login-dialog").close();
}

async function doSignIn() {
  const email = $<HTMLInputElement>("#login-email").value.trim();
  const password = $<HTMLInputElement>("#login-password").value;
  if (!email || !password) return;
  const msg = $("#login-msg");
  msg.textContent = "Đang đăng nhập…";
  try {
    session = await api.signIn(email, password);
    renderCloud();
    const then = afterLogin;
    closeLogin();
    log(`Đã đăng nhập cloud: ${session.email ?? ""}`, "ok");
    if (selected()) await loadDetail();
    then?.();
  } catch (e) {
    msg.textContent = authErrorText(e);
  }
}

async function doSignUp() {
  const email = $<HTMLInputElement>("#login-email").value.trim();
  const password = $<HTMLInputElement>("#login-password").value;
  const msg = $("#login-msg");
  if (!email || password.length < 6) {
    msg.textContent = "Nhập email và mật khẩu (ít nhất 6 ký tự).";
    return;
  }
  msg.textContent = "Đang đăng ký…";
  try {
    const r = await api.signUp(email, password);
    if (r.kind === "signed_in") {
      // Project tắt xác nhận email: có session luôn, không bắt đăng nhập lại.
      session = r.session;
      renderCloud();
      const then = afterLogin;
      closeLogin();
      log(`Đã đăng ký và đăng nhập cloud: ${session.email ?? ""}`, "ok");
      if (selected()) await loadDetail();
      then?.();
    } else if (r.kind === "needs_confirmation") {
      msg.textContent = `Đã gửi thư xác nhận tới ${r.email}. Bấm link trong thư rồi quay lại đăng nhập.`;
    } else {
      msg.textContent = "Email này đã có tài khoản — hãy đăng nhập.";
    }
  } catch (e) {
    msg.textContent = authErrorText(e);
  }
}

async function doSignOut() {
  await api.signOut();
  session = null;
  remotes = [];
  renderCloud();
  renderDetail();
  log("Đã đăng xuất cloud");
}

// ── Manifest ─────────────────────────────────────────────────────────────

async function doRefreshManifest() {
  setBusy(true);
  setStatus("Đang tải ludusavi-manifest…");
  try {
    const n = await api.refreshManifest();
    setStatus(`Đã cập nhật manifest: ${n.toLocaleString("vi-VN")} game.`, "ok");
    await refreshGames();
  } catch (e) {
    setStatus(`Không tải được manifest: ${errorText(e)}`, "error");
  } finally {
    setBusy(false);
  }
}

// ── Tiện ích ─────────────────────────────────────────────────────────────

/// Tên game và đường dẫn đến từ đĩa và từ manifest cộng đồng, nên không được
/// nhét thẳng vào innerHTML.
function escapeHtml(s: string): string {
  return s.replace(
    /[&<>"']/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!,
  );
}

void boot();
