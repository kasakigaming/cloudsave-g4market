import canyonUrl from "./assets/canyon.webp";
import dolomitesUrl from "./assets/dolomites.webp";
import galaxyUrl from "./assets/galaxy.webp";
import peakUrl from "./assets/peak.webp";
import yosemiteUrl from "./assets/yosemite.webp";
import { openUrl } from "@tauri-apps/plugin-opener";
import { convertFileSrc } from "@tauri-apps/api/core";
import { liquidSlider } from "./liquid-slider";
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
  type RestoreReport,
  type RunningGame,
  type ScanProgress,
  type Session,
  type SteamAccount,
  type SteamCloudStatus,
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

/// Thông báo ngắn dạng viên thuốc ở đáy màn hình, tự ẩn. Lỗi hiện lâu hơn
/// vì người dùng cần thời gian đọc; tiến độ ("Đang…") thì ở lại tới khi có
/// thông báo kế tiếp.
let toastTimer: number | undefined;
function setStatus(msg: string, tone: "info" | "error" | "ok" = "info") {
  const el = $("#status");
  el.textContent = msg;
  el.className = `toast toast--${tone} is-visible`;
  window.clearTimeout(toastTimer);
  const sticky = tone === "info" && /…$/.test(msg);
  if (!sticky) {
    toastTimer = window.setTimeout(() => el.classList.remove("is-visible"), tone === "error" ? 8000 : 4000);
  }
}

// ── Điều hướng ───────────────────────────────────────────────────────────

let currentView = "games";

/// Đặt vệt kính sáng lên một nút trong thanh điều hướng.
function moveGlow(target: HTMLElement) {
  const glow = $("#nav-glow");
  glow.style.width = `${target.offsetWidth}px`;
  glow.style.transform = `translateX(${target.offsetLeft}px)`;
  glow.style.opacity = "1";
}

function showView(view: string) {
  currentView = view;
  document.querySelectorAll<HTMLElement>(".view").forEach((v) =>
    v.classList.toggle("is-active", v.dataset.view === view),
  );
  document.querySelectorAll<HTMLElement>(".pillnav__item").forEach((b) =>
    b.classList.toggle("is-active", b.dataset.view === view),
  );
  if (view === "activity") {
    unread = 0;
    renderBadge();
  }
  const active = document.querySelector<HTMLElement>(".pillnav__item.is-active");
  if (active) moveGlow(active);
}

function initNav() {
  const nav = $("#nav");
  const items = nav.querySelectorAll<HTMLElement>(".pillnav__item");
  items.forEach((b) => {
    b.addEventListener("click", () => showView(b.dataset.view!));
    // Vệt sáng đi theo chuột, như mẫu anchor-positioning của Kevin Powell.
    b.addEventListener("mouseenter", () => moveGlow(b));
  });
  nav.addEventListener("mouseleave", () => {
    const active = nav.querySelector<HTMLElement>(".pillnav__item.is-active");
    if (active) moveGlow(active);
  });
  // Đặt vị trí ban đầu sau khi font đã xếp chữ xong, nếu không độ rộng sai.
  requestAnimationFrame(() => showView(currentView));
  window.addEventListener("resize", () => showView(currentView));
}

// ── Chấm báo tab Hoạt động ───────────────────────────────────────────────

let unread = 0;
function renderBadge() {
  const b = $("#activity-badge");
  b.hidden = unread === 0;
  b.textContent = unread > 9 ? "9+" : String(unread);
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
  if (currentView !== "activity" && tone !== "info") {
    unread += 1;
    renderBadge();
  }
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
  renderSteamAccount(await api.steamAccount());
  renderSteamCloud(await api.steamCloudStatus());

  if (accountId !== null) {
    await refreshGames();
    // Bước đầu tiên: kiểm tra hết save đang có và lưu vào máy.
    await runScanAll();
  }
}

// ── Hình nền ─────────────────────────────────────────────────────────────

interface Photo {
  url: string;
  /// Điểm neo khi cắt ảnh cho vừa cửa sổ.
  pos: string;
  /// Chỉ ảnh người dùng tự thêm mới có (để xoá).
  id?: string;
  /// Chỉ ảnh lấy từ web mới có: ghi công tác giả + trang gốc.
  credit?: string;
  page?: string;
}

/// Ảnh có sẵn (Unsplash, giấy phép Unsplash — tác giả ghi trong README). `pos`
/// chọn để đỉnh núi / vòm đá không bị cắt mất.
const BUILTIN: Photo[] = [
  { url: canyonUrl, pos: "center 38%" },
  { url: dolomitesUrl, pos: "center 70%" },
  { url: peakUrl, pos: "center 60%" },
  { url: yosemiteUrl, pos: "center 55%" },
  { url: galaxyUrl, pos: "center 65%" },
];
/// canyon = một ảnh đứng yên; rotate = xoay vòng ảnh từ web (không giới hạn,
/// mất mạng thì dùng ảnh có sẵn + ảnh tự thêm); mine = chỉ ảnh tự thêm.
const BACKGROUNDS = ["canyon", "rotate", "mine"];
/// Mỗi ảnh hiện bao lâu khi xoay vòng.
const ROTATE_MS = 45_000;

let rotateTimer: number | undefined;
let bgMode = "canyon";
/// Ảnh người dùng tự thêm, nạp từ backend (asset protocol).
let myPhotos: Photo[] = [];
/// URL ảnh đang hiện — để xoay tiếp đúng chỗ khi danh sách thêm / bớt ảnh.
let currentUrl = "";
/// Hàng đợi ảnh từ web; sắp hết thì lấy loạt mới, nên xoay mãi không hết.
let webQueue: Photo[] = [];
const webSeen = new Set<string>();
let webFailedAt = 0;
let webLoading: Promise<void> | null = null;
/// Lần đổi ảnh đầu tiên sau khi mở app — sớm hơn để thấy ngay ảnh từ web.
const FIRST_ROTATE_MS = 8_000;

function readLocal(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeLocal(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* không lưu được thì thôi, lần sau dùng mặc định */
  }
}

/// Lấy thêm ảnh từ web khi hàng đợi sắp hết. Lỗi mạng thì 5 phút sau mới thử
/// lại; trong lúc đó xoay ảnh có sẵn.
function refillWeb(): Promise<void> {
  if (webQueue.length >= 3 || Date.now() - webFailedAt < 5 * 60_000) return Promise.resolve();
  webLoading ??= (async () => {
    try {
      const batch = await api.webBackgrounds();
      for (const w of batch) {
        if (webSeen.has(w.url)) continue;
        webSeen.add(w.url);
        webQueue.push({
          url: w.url,
          pos: "center",
          credit: [w.author, w.license].filter(Boolean).join(" · "),
          page: w.page,
        });
      }
    } catch (e) {
      webFailedAt = Date.now();
      log(`Không tải được ảnh nền từ web: ${errorText(e)} — dùng ảnh có sẵn`, "warn");
    } finally {
      webLoading = null;
    }
  })();
  return webLoading;
}

/// Ảnh kế tiếp khi xoay: ảnh từ web nếu có, không thì vòng qua ảnh có sẵn.
async function upcomingPhoto(): Promise<Photo> {
  if (bgMode === "rotate") {
    await refillWeb();
    const p = webQueue.shift();
    void refillWeb();
    if (p) return p;
  }
  return nextPhoto();
}

/// Bộ ảnh đang xoay. Chưa thêm ảnh nào mà chọn "ảnh của bạn" thì dùng ảnh có sẵn.
function pool(): Photo[] {
  if (bgMode === "mine" && myPhotos.length > 0) return myPhotos;
  if (bgMode === "canyon") return [BUILTIN[0]];
  return [...BUILTIN, ...myPhotos];
}

/// Hiện ảnh lên lớp đang ẩn rồi cho nó hiện dần lên trên lớp kia.
/// Đợi ảnh giải mã xong mới đổi, để không chớp nền trống.
async function showPhoto(photo: Photo, instant = false) {
  currentUrl = photo.url;
  const img = new Image();
  img.src = photo.url;
  // Chờ tối đa 3 giây: ảnh lỗi hay giải mã treo thì vẫn đổi, trình duyệt tự
  // vẽ khi tải xong — không để việc xoay vòng đứng hẳn.
  await Promise.race([img.decode().catch(() => undefined), new Promise((r) => setTimeout(r, 3000))]);
  if (currentUrl !== photo.url) return; // đã có lệnh đổi ảnh khác trong lúc chờ
  const layers = document.querySelectorAll<HTMLElement>(".backdrop__img");
  const current = [...layers].find((l) => l.classList.contains("is-on")) ?? layers[0];
  const next = instant ? current : [...layers].find((l) => l !== current) ?? current;
  next.style.backgroundImage = `url("${photo.url}")`;
  next.style.backgroundPosition = photo.pos;
  if (next !== current) {
    // Khởi động lại hiệu ứng phóng chậm cho ảnh mới.
    next.style.animation = "none";
    void next.offsetWidth;
    next.style.animation = "";
    next.classList.add("is-on");
    current.classList.remove("is-on");
  }
  writeLocal("cloudsave.bg.url", photo.url);
  renderCredit(photo);
  renderMyPhotos();
}

function renderCredit(photo: Photo) {
  const el = $<HTMLAnchorElement>("#bg-credit");
  el.hidden = !photo.credit;
  el.textContent = photo.credit ? `Ảnh: ${photo.credit} · Wikimedia Commons` : "";
  el.dataset.page = photo.page ?? "";
  el.title = "Mở trang gốc của ảnh";
}

/// Ảnh kế tiếp sau ảnh đang hiện; vòng lại từ đầu, không bao giờ dừng.
function nextPhoto(): Photo {
  const list = pool();
  const i = list.findIndex((p) => p.url === currentUrl);
  return list[(i + 1) % list.length];
}

/// Lựa chọn hình nền là tiện ích của riêng máy này, nên để trong localStorage.
function applyBackground(bg: string) {
  bgMode = BACKGROUNDS.includes(bg) ? bg : "canyon";
  document.body.dataset.bg = bgMode === "canyon" ? "canyon" : "rotate";
  $<HTMLSelectElement>("#bg-select").value = bgMode;
  writeLocal("cloudsave.bg", bgMode);
  window.clearInterval(rotateTimer);
  window.clearTimeout(rotateTimer);
  rotateTimer = undefined;

  if (bgMode === "canyon") {
    void showPhoto(BUILTIN[0], true);
    return;
  }
  // Hiện ngay một ảnh có sẵn (tiếp sau ảnh lần trước) để không có nền trống,
  // rồi chuyển sang ảnh từ web khi tải xong.
  currentUrl = readLocal("cloudsave.bg.url") ?? "";
  void showPhoto(nextPhoto(), true);
  const advance = async () => {
    const mode = bgMode;
    const p = await upcomingPhoto();
    if (mode === bgMode) void showPhoto(p);
  };
  rotateTimer = window.setTimeout(() => {
    void advance();
    rotateTimer = window.setInterval(() => void advance(), ROTATE_MS);
  }, FIRST_ROTATE_MS);
}

// ── Ảnh nền tự thêm ──────────────────────────────────────────────────────

async function loadMyPhotos() {
  try {
    const list = await api.listBackgrounds();
    myPhotos = list.map((b) => ({ url: convertFileSrc(b.path), pos: "center", id: b.id }));
  } catch (e) {
    log(`Không đọc được ảnh nền của bạn: ${errorText(e)}`, "error");
    myPhotos = [];
  }
  renderMyPhotos();
}

function renderMyPhotos() {
  const box = $("#bg-thumbs");
  box.innerHTML = myPhotos
    .map(
      (p) => `<div class="bg-thumb${p.url === currentUrl ? " is-current" : ""}" data-url="${escapeHtml(p.url)}"
        style="background-image:url('${escapeHtml(p.url)}')" title="Bấm để hiện ngay">
        <button type="button" class="bg-thumb__x" data-id="${escapeHtml(p.id ?? "")}" aria-label="Xoá ảnh này">×</button>
      </div>`,
    )
    .join("");
  box.hidden = myPhotos.length === 0;
  $("#bg-mine-hint").textContent =
    myPhotos.length === 0
      ? "Thêm ảnh từ máy để xoay vòng cùng ảnh có sẵn — bao nhiêu ảnh cũng được."
      : `${myPhotos.length} ảnh — bấm vào ảnh để hiện ngay, × để xoá.`;
  const mine = $<HTMLSelectElement>("#bg-select").querySelector<HTMLOptionElement>('option[value="mine"]');
  if (mine) mine.disabled = myPhotos.length === 0;
}

async function addMyPhotos() {
  try {
    const r = await api.pickBackgrounds();
    if (r.added.length === 0 && r.skipped.length === 0) return; // bấm huỷ
    await loadMyPhotos();
    if (r.added.length > 0) {
      setStatus(`Đã thêm ${r.added.length} ảnh nền`, "ok");
      // Đang để một ảnh đứng yên thì chuyển sang xoay vòng để thấy ảnh mới.
      if (bgMode === "canyon") applyBackground("rotate");
      const first = myPhotos.find((p) => p.id === r.added[0].id);
      if (first) void showPhoto(first);
    }
    for (const s of r.skipped) log(`Bỏ qua ảnh ${s}`, "warn");
  } catch (e) {
    setStatus(`Không thêm được ảnh: ${errorText(e)}`, "error");
  }
}

async function removeMyPhoto(id: string) {
  try {
    await api.removeBackground(id);
    const removed = myPhotos.find((p) => p.id === id);
    await loadMyPhotos();
    if (bgMode === "mine" && myPhotos.length === 0) applyBackground("rotate");
    else if (removed && removed.url === currentUrl) void showPhoto(nextPhoto());
  } catch (e) {
    setStatus(`Không xoá được ảnh: ${errorText(e)}`, "error");
  }
}

function initMyPhotos() {
  $("#btn-add-bg").addEventListener("click", () => void addMyPhotos());
  $("#bg-credit").addEventListener("click", (e) => {
    e.preventDefault();
    const page = (e.currentTarget as HTMLElement).dataset.page;
    if (page?.startsWith("https://commons.wikimedia.org/")) void openUrl(page);
  });
  $("#bg-thumbs").addEventListener("click", (e) => {
    const target = e.target as HTMLElement;
    const x = target.closest<HTMLElement>(".bg-thumb__x");
    if (x?.dataset.id) {
      e.stopPropagation();
      void removeMyPhoto(x.dataset.id);
      return;
    }
    const thumb = target.closest<HTMLElement>(".bg-thumb");
    const photo = myPhotos.find((p) => p.url === thumb?.dataset.url);
    if (!photo) return;
    if (bgMode === "canyon") applyBackground("mine");
    void showPhoto(photo);
  });
}

/// Độ trong suốt của kính, 0–100. 50 là giao diện mặc định (kính mờ 16px, phủ tối
/// 22%); 100 gần như trong suốt để ngắm nền, 0 đục hẳn để dễ đọc chữ.
function applyGlass(clarity: number) {
  const t = Math.min(100, Math.max(0, clarity)) / 100;
  const alpha = 0.4 - 0.36 * t;
  const blur = Math.round(30 - 30 * t);
  const body = document.body.style;
  body.setProperty("--glass", `rgb(20 10 30 / ${alpha.toFixed(3)})`);
  body.setProperty("--glass-blur", `blur(${blur}px) saturate(${blur > 0 ? 160 : 110}%)`);
  $("#glass-value").textContent = `${clarity}%`;
}

function initGlass() {
  const saved = Number(readLocal("cloudsave.glass") ?? 50);
  const start = Number.isFinite(saved) ? saved : 50;
  applyGlass(start);
  liquidSlider($("#glass-range"), {
    min: 0,
    max: 100,
    step: 1,
    value: start,
    onInput: (v) => {
      applyGlass(v);
      writeLocal("cloudsave.glass", String(v));
    },
  });
}

function initBackground() {
  initGlass();
  initMyPhotos();
  applyBackground(readLocal("cloudsave.bg") ?? "canyon");
  // Ảnh tự thêm nạp sau (cần gọi backend); nạp xong thì chế độ "ảnh của bạn"
  // mới có ảnh để xoay.
  void loadMyPhotos().then(() => {
    if (bgMode === "mine") applyBackground("mine");
  });
  $<HTMLSelectElement>("#bg-select").addEventListener("change", (e) =>
    applyBackground((e.target as HTMLSelectElement).value),
  );
}

function wireEvents() {
  initBackground();
  initNav();
  initSteamCloud();
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
  await api.on<SteamAccount | null>("steam-account", (a) => void onSteamAccount(a));
  await api.on<SteamCloudStatus>("steam-cloud", onSteamCloud);
  await api.on<string>("steam-cloud-ui", (msg) => {
    setStatus(msg, "info");
    log(msg, "info");
    if (steamCloud) renderSteamCloud({ ...steamCloud, busy: true });
  });
  await api.on<string>("steam-cloud-ui-error", (msg) => {
    setStatus(`Không tắt được Steam Cloud: ${msg}`, "error");
    log(`Không tắt được Steam Cloud tự động: ${msg}`, "error");
  });
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

// ── Tài khoản Steam đang đăng nhập ──────────────────────────────────────

function renderSteamAccount(a: SteamAccount | null) {
  const chip = $("#steam-chip");
  if (!a) {
    chip.hidden = true;
    return;
  }
  chip.hidden = false;
  const name = a.persona_name || a.account_name || `Tài khoản ${a.account_id}`;
  chip.className = `acct ${a.logged_in ? "acct--on" : "acct--off"}`;
  $("#steam-name").textContent = a.logged_in ? name : `Steam đã tắt · ${name}`;
  const avatar = $("#steam-avatar");
  avatar.innerHTML = a.avatar
    ? `<img src="${a.avatar}" alt="" />`
    : escapeHtml(name.trim().charAt(0).toUpperCase() || "?");
  chip.title = [
    a.logged_in ? "Steam đang đăng nhập" : "Steam không chạy — tài khoản dùng gần nhất",
    `${name}${a.account_name && a.account_name !== name ? ` (${a.account_name})` : ""}`,
    `ID ${a.account_id}`,
  ].join("\n");
}

// ── Steam Cloud ─────────────────────────────────────────────────────────

let steamCloud: SteamCloudStatus | null = null;

function renderSteamCloud(s: SteamCloudStatus) {
  steamCloud = s;
  const chip = $<HTMLButtonElement>("#steam-cloud");
  chip.hidden = s.account_id === null;
  const state = s.busy ? "busy" : s.state;
  chip.className = `scloud scloud--${state}`;
  chip.title = {
    busy: "Đang đổi Steam Cloud…",
    unknown: `Không đọc được Steam Cloud${s.error ? `: ${s.error}` : ""}`,
    on: "Steam Cloud đang BẬT",
    queued: "Steam Cloud còn bật — sẽ tắt ở lần đăng nhập tới",
    off: "Steam Cloud đã tắt",
  }[state];

  $("#scloud-title").textContent = {
    busy: "Đang đổi Steam Cloud…",
    unknown: "Không đọc được Steam Cloud",
    on: "Steam Cloud đang bật",
    queued: "Steam Cloud sẽ tắt ở lần đăng nhập tới",
    off: "Steam Cloud đã tắt",
  }[state];
  $("#scloud-title").className = `scloud-pop__title scloud-pop__title--${state}`;

  const why = "Steam có thể đồng bộ đè bản save cũ trên cloud của nó lên save CloudSave vừa khôi phục.";
  const now = s.logged_in
    ? " Bấm tắt: app mở Settings → Cloud của Steam và gạt công tắc giúp bạn (mượn chuột 1–2 giây, không khởi động lại Steam)."
    : " Tài khoản này đang không đăng nhập nên tắt xong là có hiệu lực ở lần đăng nhập tới.";
  $("#scloud-text").textContent = {
    busy: s.logged_in
      ? "Đang gạt công tắc trong cửa sổ Settings của Steam — đừng động vào chuột."
      : "Đang ghi cài đặt…",
    unknown: s.error ?? "Chưa xác định được tài khoản Steam.",
    on:
      why +
      (s.auto_disable && s.logged_in
        ? " App đang tự động: sẽ gạt công tắc trong Settings của Steam khi không có game nào chạy."
        : now),
    queued:
      "App đã ghi sẵn cài đặt tắt. Steam chỉ đọc lúc đăng nhập, nên lần đăng nhập tới tài khoản này sẽ tắt Steam Cloud.",
    off: "CloudSave là nơi giữ save duy nhất, không bị Steam ghi đè.",
  }[state];

  const btn = $<HTMLButtonElement>("#btn-scloud-toggle");
  const wantsOff = state === "on" || state === "queued" || state === "busy";
  btn.hidden = state === "unknown" || (state === "queued" && !s.logged_in);
  btn.disabled = s.busy || (state === "off" && s.auto_disable);
  btn.className = wantsOff ? "danger" : "ghost";
  btn.textContent = !wantsOff ? "Bật lại" : "Tắt Steam Cloud";
  // Đang đăng nhập thì app không bật lại hộ được (phải gạt trong Steam).
  if (!wantsOff && s.logged_in) btn.hidden = true;
  btn.title = state === "off" && s.auto_disable ? "Đang tự động tắt — bỏ chọn bên dưới trước" : "";

  $<HTMLInputElement>("#scloud-auto").checked = s.auto_disable;
  $<HTMLInputElement>("#setting-scloud-auto").checked = s.auto_disable;
}

function onSteamCloud(s: SteamCloudStatus) {
  const prev = steamCloud;
  renderSteamCloud(s);
  if (!prev || prev.account_id !== s.account_id || prev.state === s.state) return;
  if (s.state === "on") log("Steam Cloud đang BẬT — có thể đè save đã khôi phục", "warn");
  else if (s.state === "queued") log("Đã hẹn tắt Steam Cloud ở lần đăng nhập tới", "info");
  else if (s.state === "off") log("Steam Cloud đã tắt", "ok");
}

async function toggleSteamCloud() {
  if (!steamCloud || steamCloud.state === "unknown") return;
  const want = steamCloud.state === "off";
  renderSteamCloud({ ...steamCloud, busy: true });
  try {
    renderSteamCloud(await api.setSteamCloud(want));
    setStatus(want ? "Đã bật lại Steam Cloud" : "Đã tắt Steam Cloud", "ok");
  } catch (e) {
    setStatus(`Không đổi được Steam Cloud: ${errorText(e)}`, "error");
    log(`Không đổi được Steam Cloud: ${errorText(e)}`, "error");
    renderSteamCloud(await api.steamCloudStatus());
  }
}

async function setAutoDisable(on: boolean) {
  try {
    renderSteamCloud(await api.setAutoDisableSteamCloud(on));
  } catch (e) {
    setStatus(`Không lưu được tuỳ chọn: ${errorText(e)}`, "error");
  }
}

function initSteamCloud() {
  const chip = $("#steam-cloud");
  const pop = $("#scloud-pop");
  chip.addEventListener("click", (e) => {
    e.stopPropagation();
    pop.hidden = !pop.hidden;
  });
  pop.addEventListener("click", (e) => e.stopPropagation());
  document.addEventListener("click", () => (pop.hidden = true));
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") pop.hidden = true;
  });
  $("#btn-scloud-toggle").addEventListener("click", () => void toggleSteamCloud());
  for (const id of ["#scloud-auto", "#setting-scloud-auto"]) {
    $(id).addEventListener("change", (e) => void setAutoDisable((e.target as HTMLInputElement).checked));
  }
}

/// Steam vừa đổi tài khoản (hoặc bật / tắt). Nếu là tài khoản khác thì đổi
/// theo và quét lại — save của mỗi tài khoản nằm ở chỗ khác nhau.
async function onSteamAccount(a: SteamAccount | null) {
  renderSteamAccount(a);
  if (!a || !a.logged_in || a.account_id === accountId) return;
  const name = a.persona_name || a.account_name || String(a.account_id);
  log(`Steam đổi sang tài khoản ${name} — quét lại save`, "ok");
  accountId = a.account_id;
  selectedAppId = null;
  if (!users.some((u) => u.account_id === a.account_id)) {
    users.push({
      account_id: a.account_id,
      steam_id64: a.steam_id64,
      account_name: a.account_name,
      persona_name: a.persona_name,
      last_login: Date.now() / 1000,
    });
  }
  renderUsers();
  renderDetail();
  await refreshGames();
  await runScanAll();
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
      <button id="btn-capture" class="primary" data-guard ${g.running ? "disabled" : ""}
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
            <button class="ghost ghost--accent" data-guard data-push="${s.id}" ${!cloudConfigured ? "disabled title='Chưa cấu hình Supabase'" : ""}>
              ${s.remote_id ? "Đẩy lại" : "Đẩy lên cloud"}
            </button>
            <button class="ghost" data-guard data-restore-local="${s.id}" ${g.running ? "disabled" : ""}>Khôi phục</button>
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
              <button class="ghost" data-guard data-restore-remote="${r.id}" ${g.running ? "disabled" : ""}>Khôi phục</button>
              <button class="ghost danger" data-guard data-delete-remote="${r.id}">Xoá</button>
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

/// Kể lại phần "khôi phục cho tài khoản nào" của báo cáo.
///
/// Đáng nói với người dùng vì đây là thứ quyết định save có trụ được không:
/// Steam dời sạch file của tài khoản khác sang `userdata/<tài khoản đó>/…/ac`
/// ngay lần quét kế tiếp.
function logRestoreAccount(r: RestoreReport) {
  // Câu "bản lưu thuộc tài khoản nào" đã có trong `warnings` từ backend; ở đây
  // chỉ báo kết quả.
  if (r.remapped) {
    log(`↔ Đã chuyển ${r.remapped} file sang thư mục của tài khoản ${r.target_account}`, "ok");
  }
  if (r.markers) log(`✓ Đã báo Steam chỗ save này thuộc tài khoản ${r.target_account}`, "ok");
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
    logRestoreAccount(r);
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
    await reconcileCloud();
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
    const where = r.safety_dir ? ` Bản cũ ở: ${r.safety_dir}` : "";
    setStatus(`Đã khôi phục ${r.restored} file từ cloud.${where}`, "ok");
    log(`↺ Khôi phục từ cloud: ${r.restored} file${r.skipped ? `, bỏ qua ${r.skipped}` : ""}`, "ok");
    logRestoreAccount(r);
    for (const w of r.warnings) log(w, "warn");
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
    await reconcileCloud();
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
    await reconcileCloud();
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

/// Gỡ dấu "đã lên cloud" của các bản local mà bản trên cloud đã bị xoá (trong
/// app, trong Dashboard hay từ máy khác), rồi vẽ lại danh sách.
async function reconcileCloud() {
  try {
    const n = await api.reconcileCloud();
    if (n > 0) log(`Đã gỡ dấu cloud của ${n} bản không còn trên cloud`, "info");
  } catch (e) {
    console.warn("không đối chiếu được cloud:", e);
  }
  await refreshGames();
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
