# CloudSave G4Market

Sao lưu save game lên Supabase. Đọc đường dẫn save từ chính dữ liệu Steam trên
đĩa, không hook vào tiến trình Steam.

## Luồng hoạt động

**Không cần đăng nhập.** Mọi thứ dưới đây chạy offline:

1. **Mở app → tự quét hết save** của tài khoản Steam đăng nhập gần nhất, lưu
   vào kho trên máy (`%LOCALAPPDATA%\cloudsave-g4market\store`). Game nào save
   không đổi so với bản gần nhất thì không sinh bản mới.
2. **Watcher biết game nào đang chơi.** Mỗi 2 giây nó đọc registry của Steam
   (`RunningAppID`, `Apps\<appid>\Running`) và danh sách tiến trình (exe nằm
   trong thư mục cài game — bắt được cả game mở ngoài Steam).
3. **Game tắt → chụp save ngay.** Đợi 3 giây cho game ghi nốt file rồi chụp vào
   kho local. Đo trên app thật: bản chụp có mặt 4,9 giây sau khi game tắt.
4. **Khôi phục từ máy** bất kỳ bản nào, không cần mạng. Bị chặn nếu game đang
   chạy.

**Cloud chỉ khi bấm.** Nút "Đẩy lên cloud" trên từng bản local mới hỏi đăng
nhập, và đẩy **đúng bản đã chụp** chứ không quét lại đĩa — thứ bạn thấy trong
danh sách là thứ lên cloud.

## Ý tưởng

Ba quyết định thiết kế, và lý do đằng sau mỗi cái.

### 1. Đường dẫn save lấy từ Steam, không phải đoán

Steam tự khai báo chỗ để save của từng game trong `appcache/appinfo.vdf`, nhánh
`ufs.savefiles`. Đây chính là dữ liệu mà Steam Cloud dùng, và đọc được hoàn
toàn offline.

Ba nguồn, xếp theo độ tin cậy giảm dần:

| Nguồn | Nội dung | Vì sao dùng |
|---|---|---|
| `appcache/appinfo.vdf` → `ufs.savefiles` | `root` + `path` + `pattern` + `recursive` | Chính Steam khai, chính xác nhất |
| `userdata/<acc>/<app>/remotecache.vdf` | file đã sync, kèm size / SHA-1 / mtime | Xác nhận file có thật, và cho change-detection miễn phí |
| [ludusavi-manifest] | ~19.000 game từ PCGamingWiki | Phủ game không bật Cloud và game non-Steam |

[ludusavi-manifest]: https://github.com/mtkennerly/ludusavi-manifest

### 2. Lưu `(root_token, rel_path)`, không lưu đường dẫn tuyệt đối

Ludusavi lưu nguyên `C:\Users\foo\AppData\Roaming\...` và tự nhận đó là lựa
chọn thận trọng. Ta lưu `WinAppDataRoaming` + `StardewValley/Saves/...` thay
vào đó, nên khôi phục sang máy có tên người dùng khác hoặc ổ đĩa khác vẫn ra
đúng chỗ.

Bảng root token, phần đã đối chiếu với đĩa thật:

| Root id (`remotecache.vdf`) | Token | Trỏ tới |
|---|---|---|
| `0` | `SteamCloudRemote` | `<steam>/userdata/<acc>/<app>/remote/` |
| `1` | `GameInstall` | thư mục cài game |
| `4` | `WinAppDataRoaming` | `%APPDATA%` |
| `18` | `WinUserHome` | `%USERPROFILE%` |

Các id còn lại (`2`, `3`, `9`, `12`) lấy từ tài liệu cộng đồng và **chưa đối
chiếu được**. Gặp id lạ thì app bỏ qua file đó thay vì đoán — đoán sai root
nghĩa là khôi phục save vào nhầm thư mục.

`WinMyDocuments` phân giải qua registry `Shell Folders` chứ không ghép
`%USERPROFILE%\Documents`, để tôn trọng chuyển hướng OneDrive.

### 3. Bytes là nguồn chân lý; `preview` chỉ để nhìn

Nội dung file đi vào Postgres dưới dạng `bytea`, nén zstd, cắt chunk 512 KiB,
content-addressed theo SHA-256 của nội dung gốc.

App **không** parse save thành dữ liệu có nghĩa rồi dựng lại file từ đó. Save
game thường có checksum nội bộ, mã hoá theo máy, hoặc bố cục phụ thuộc phiên
bản engine; round-trip không byte-exact là save hỏng.

Đổi lại, cột `snapshots.preview` chứa những gì đọc được từ các format dễ (JSON,
INI, XML) để UI hiển thị. Nó **không bao giờ** tham gia vào việc khôi phục —
xem [`preview.rs`](src-tauri/src/preview.rs).

## Chạy thử

### Yêu cầu

Rust (stable, MSVC) và Node. Nếu máy chưa có Rust:

```powershell
winget install Rustlang.Rustup
# Chỉ cần nếu chưa có MSVC linker:
winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

### Cloud (tuỳ chọn)

App chạy đầy đủ mà **không cần cloud**: quét, lưu vào máy, theo dõi game và
khôi phục đều offline. Không có `.env` thì nút đẩy lên cloud chỉ đơn giản là tắt.

Tính năng cloud dùng Supabase (Auth + Postgres). **Schema backend không nằm
trong repo này.** Muốn bật cloud, bạn cần một project Supabase có schema tương
thích với [`supabase.rs`](src-tauri/src/supabase.rs), rồi copy `.env.example`
thành `.env` và điền `SUPABASE_URL` cùng `SUPABASE_ANON_KEY`.

`SUPABASE_ANON_KEY` **phải** là publishable key (`sb_publishable_…`) hoặc anon
key kiểu cũ. Khoá này công khai được — Row Level Security mới là thứ bảo vệ dữ
liệu. Lúc build, [`build.rs`](src-tauri/build.rs) nhúng nó vào exe để bộ cài
không cần kèm `.env`.

Tuyệt đối không đặt secret / `service_role` key vào đó:

- App desktop phát tán cấu hình **cùng file exe**, nên secret key ở đó trao toàn
  quyền database cho bất kỳ ai cầm bản build.
- App sẽ hỏng ngay: `service_role` bypass RLS nên `auth.uid()` trả NULL và mọi
  INSERT gắn với người dùng đều fail.

`Config::from_env()` chặn sẵn: nó từ chối khởi động nếu phát hiện `sb_secret_…`
hoặc JWT có `role: service_role`, và `build.rs` không bao giờ nhúng secret key.

### Chạy

```powershell
npm install
npm run tauri dev
```

## Kiểm thử

```powershell
cd src-tauri
cargo test                                                    # 43 unit test + watcher_real
cargo test --test scan_real_steam -- --ignored --nocapture    # chạy trên Steam thật
cargo clippy --all-targets
```

Bộ integration test là thứ đáng chạy nhất — nó đi hết chuỗi từ `appinfo.vdf`
nhị phân tới một file có thật trên đĩa, và khẳng định ba bất biến:

- **Không root nào bị bỏ sót.** Mọi tên root Steam dùng trong `ufs.savefiles`
  phải map được, nếu không app đang âm thầm bỏ qua save của game đó.
- **Mọi file tìm ra phải tồn tại**, và `root + rel_path` phải ghép lại đúng
  bằng đường dẫn tuyệt đối — đây là điều kiện để khôi phục sang máy khác chạy đúng.
- **Round-trip byte-exact** qua sha256 + zstd + chunk, trên save thật.

Kết quả trên máy phát triển (93 tài khoản Steam, 1 thư viện):

```
app trong cache : 2420        app khai ufs : 221        root chưa map : []
quét           : 107 game, 680 file, 1.078.343.448 byte
round-trip     : byte-exact trên 680 file thật (~1,08 GB)
```

### Watcher với tiến trình thật

[`watcher_real.rs`](src-tauri/tests/watcher_real.rs) chép `cmd.exe` vào một
thư mục game giả, chạy nó, và đòi watcher nhận ra — rồi tắt và đòi watcher thấy
nó biến mất. Chạy trong `cargo test` thường, không cần Steam.

### Test end-to-end thật

[`supabase_e2e.rs`](src-tauri/tests/supabase_e2e.rs) chạy cả vòng Steam →
Supabase → đĩa. **Nó xoá file save thật rồi khôi phục**, nên hãy sao lưu trước
khi chạy lần đầu.

```powershell
$env:CS_TEST_EMAIL="..."; $env:CS_TEST_PASSWORD="..."
cargo test --test supabase_e2e -- --ignored --nocapture --test-threads=1
```

Kết quả thật trên save Stardew Valley 11 MB:

```
[1]  quét            : 7 file, 11.029.616 byte
[2]  chụp local      : 7 file — chưa lên cloud
[3]  chụp lại        : không đổi → không ghi thêm
[5]  đẩy lên cloud   : tải lên 11.029.616 byte
[6]  đẩy lại         : tải lên 0 byte
[7]  xung đột        : máy khác bị chặn đúng
[8]  XOÁ → khôi phục từ LOCAL : byte-exact 11.029.616 byte (không dùng mạng)
[9]  XOÁ → khôi phục từ CLOUD : byte-exact 11.029.616 byte
[10] dọn cloud       : gc giải phóng 26 chunk
```

Test tự dọn snapshot cũ ở bước `[1b]` nên chạy lại được nhiều lần; nhờ vậy
bước `[5]` luôn là một lần upload thật chứ không ăn sẵn blob của lần trước.

## Cấu trúc

```
src-tauri/src/
├─ steam/
│  ├─ appinfo.rs      parser binary VDF (magic 0x07564429, có string table)
│  ├─ remotecache.rs  parser text VDF + kiểm tra path traversal
│  ├─ locate.rs       dò Steam root, thư viện, tài khoản
│  ├─ roots.rs        root token → đường dẫn tuyệt đối
│  └─ textvdf.rs      parser KeyValues dạng text
├─ manifest/ludusavi.rs   tải + cache manifest, map placeholder → root token
├─ scan.rs            gộp ba nguồn thành danh sách file, dedupe
├─ local_store.rs     kho trên máy: blob nén theo sha256 + snapshot JSON
├─ watcher.rs         game đang chạy (registry + tiến trình), chụp khi tắt
├─ blob.rs            sha256 + zstd + chunk, round-trip byte-exact
├─ preview.rs         trích data để hiển thị (không dùng khi khôi phục)
├─ supabase.rs        GoTrue + PostgREST + RPC bytea
├─ backup.rs          đẩy một bản LOCAL: dedupe → upload → metadata → complete
├─ restore.rs         từ local hoặc cloud: verify → cứu hộ → tmp-then-rename
└─ commands.rs        lệnh Tauri

src-tauri/tests/
├─ scan_real_steam.rs integration test chạy trên Steam thật (#[ignore])
├─ watcher_real.rs    phát hiện tiến trình game thật
└─ supabase_e2e.rs    local → cloud → xoá đĩa → khôi phục (#[ignore], phá huỷ)
```

## Những chỗ cần biết trước khi dùng thật

- **Xung đột không tự giải.** Nếu một máy khác đã tạo bản mới hơn, app dừng và
  hỏi. Save game không merge được nên đây là quyết định của người dùng.
- **Đóng game trước khi sao lưu.** File đang bị game khoá ghi sẽ bị bỏ qua và
  báo trong cảnh báo.
- **Khôi phục luôn cất bản cũ trước** vào
  `%LOCALAPPDATA%\cloudsave-g4market\pre-restore\`. Nếu không cất được thì
  không ghi đè.
- **Dung lượng.** Postgres free tier có 500 MB. File lớn hơn 64 MB bị bỏ qua;
  loại đó nên đi Supabase Storage. Xoá snapshot xong nhớ để `cs_gc_blobs()`
  chạy — nó được gọi tự động sau mỗi lần xoá.
- **Registry chưa hỗ trợ.** Một số game cũ lưu save trong registry; ludusavi có
  backup phần này, bản v1 ở đây thì chưa.

## Về OpenSteamTool / CloudRedirect

Commit `00984b4` của OpenSteamTool hoạt động theo mô hình khác hẳn: nó inject
`cloud_redirect.dll` vào tiến trình Steam, chặn gói `ServiceMethodCallFromClient`
có `target_job_name` bắt đầu bằng `"Cloud."`, và tự chế `ServiceMethodResponse`
để giả lập backend Steam Cloud.

App này không làm vậy. Nó chạy ngoài Steam và chỉ đọc file — an toàn hơn nhiều,
và vẫn có đúng thông tin đường dẫn vì cả hai đều lấy từ cùng một nguồn là cấu
hình UFS của Steam.

Cách hook chỉ cần thiết khi game phải *tin rằng* Steam Cloud đang hoạt động.
