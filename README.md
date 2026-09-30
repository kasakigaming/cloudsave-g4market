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

**Đăng nhập cloud là thấy mọi game đã đẩy lên**, kể cả game chưa cài trên máy
này (nhãn "chưa cài"). Bản trên cloud được tìm theo appid Steam, không theo tên
game — tên do máy đẩy lên đặt và máy khác có thể đặt khác. Save nằm trong
AppData / Documents / `userdata` khôi phục được trước khi cài game; save nằm
trong thư mục cài game thì phải cài game trước.

**Theo tài khoản Steam đang đăng nhập.** Chip trên thanh trên cùng hiện tài
khoản Steam đang đăng nhập (đọc `ActiveProcess\ActiveUser` trong registry).
Đổi tài khoản trong Steam thì app đổi theo và quét lại save ngay.

### Tắt Steam Cloud

Steam Cloud bật song song với CloudSave là hai bên cùng giữ một bộ save: khôi
phục xong, Steam có thể đồng bộ đè bản cũ trên cloud của nó xuống. Chip
"Steam Cloud" báo đỏ khi đang bật, xanh khi đã tắt. Bật "Tự động tắt Steam
Cloud" (Cài đặt) thì:

- **Tài khoản đang đăng nhập:** app mở Settings → Cloud của Steam, đọc màn hình
  bằng OCR có sẵn của Windows và gạt công tắc — có hiệu lực ngay, không khởi
  động lại Steam, Steam tự đẩy cài đặt lên server của nó. Giao diện Steam là
  Chromium đã tắt accessibility, nên không có cách nào khác ngoài "nhìn" màn
  hình (`steam_ui.rs`). Mỗi lần bấm đều chụp lại để kiểm tra; không chắc thì
  dừng. Không làm khi đang chơi game.
- **Các tài khoản khác trên máy:** app ghi sẵn `CloudEnabled "0"` vào
  `userdata/<id>/7/remote/sharedconfig.vdf` và vào "write-aside store"
  `userdata/<id>/config/sharedconfig.vdf` mà Steam gộp vào lúc đăng nhập
  (`steam_cloud.rs`). Chỉ sửa file trong `remote/` là không đủ: Steam lấy bản
  trên server đè xuống. Bản gốc được giữ ở
  `%LOCALAPPDATA%\cloudsave-g4market\steam-backup\`.

Thử riêng phần OCR, không bấm gì: `cargo run --example steam_cloud_ui`.

### Khôi phục cho tài khoản Steam đang đăng nhập

Steam đặt ở gốc mỗi thư mục save một file `steam_autocloud.vdf` ghi tài khoản
nào đang sở hữu chỗ đó:

```
"steam_autocloud.vdf"
{
    "accountid"        "111111111"
}
```

Tài khoản khác đăng nhập là AutoCloud **dời** (move, không phải copy) toàn bộ
file khớp quy tắc UFS sang `userdata/<accountid cũ>/<appid>/ac`, rồi kéo bản của
tài khoản mới từ `ac` của chính nó về. Log của Steam trên máy phát triển:

```
AutoCloud found files for previous user 333333333 in root ...\StardewValley\Saves
AutoCloud saved (move) ...\Saves\farm_400000001\farm_400000001
  to ...\userdata\333333333\413150\ac\WinAppDataRoaming\StardewValley\Saves\...
AutoCloud restoring files from ...\userdata\111111111\413150\ac
```

Đây là lý do "khôi phục xong Steam xoá mất save": file không mất, nó nằm trong
`ac` của tài khoản cũ. Ba việc app làm để khỏi dính (`steam/autocloud.rs`):

1. **Đích là tài khoản đang đăng nhập**, đọc từ registry `ActiveUser`, chứ không
   phải tài khoản đã chụp bản lưu. Steam tắt thì dùng tài khoản đăng nhập gần
   nhất; không biết tài khoản nào thì dừng, không đoán.
2. **Đổi id tài khoản nằm trong đường dẫn.** Khoảng 19% quy tắc UFS có
   `{64BitSteamID}` / `{Steam3AccountID}`, app giải placeholder ngay lúc quét nên
   `rel_path` mang sẵn id của tài khoản đã chụp (vd
   `SB/Saved/SaveGames/76561198071376839/…`). Chỉ đổi **cả đoạn** đường dẫn: tên
   save do game tự sinh cũng chứa số (`farm_400000001` của Stardew) và không liên
   quan tới tài khoản.
3. **Dán lại nhãn `steam_autocloud.vdf`** sang tài khoản đích. App không tạo file
   này ở chỗ Steam chưa từng đặt, và cũng không bao giờ khôi phục lại nội dung cũ
   của nó — ghi đè nhãn tài khoản cũ lên chỗ save là tự bắn vào chân.

Quét cũng bỏ qua `steam_autocloud.vdf`: nó là sổ sách của Steam, không phải dữ
liệu chơi.

Đổi id nào thì quyết định dựa vào tài khoản đang đăng nhập (`decide_remap` trong
`restore.rs`):

- **Bản lưu đã có sẵn thư mục của tài khoản đang đăng nhập** thì không đổi gì.
  Một game chơi bằng hai tài khoản trên cùng máy thì bản chụp chứa cả hai thư
  mục; đổi thư mục kia sang tài khoản đang đăng nhập là hai bộ save đè nhau.
- Bản đẩy lên cloud có kèm `steam_account_id` → đổi id của tài khoản đó.
- Bản đẩy từ app cũ (không có) → dò SteamID64 trong đường dẫn: có một id thì
  chính nó, có nhiều id thì lấy thư mục có file mới nhất. Chỉ đổi đúng một thư
  mục, thư mục của tài khoản đích chưa có thì tạo mới. Dò ra chắc chắn (một id)
  thì ghi `steam_account_id` ngược lên cloud cho lần sau.
- Steam3 id (số 32-bit trần) chỉ đổi khi biết tài khoản nguồn — dò bằng hình
  dạng thì dễ nhầm với số do game tự sinh.

## Ý tưởng

Bốn quyết định thiết kế, và lý do đằng sau mỗi cái.

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

App **không** parse save thành dữ liệu có nghĩa rồi dựng lại file từ đó. Save
game thường có checksum nội bộ, mã hoá theo máy, hoặc bố cục phụ thuộc phiên
bản engine; round-trip không byte-exact là save hỏng. Mọi blob đều được kiểm
SHA-256 trước khi ghi ra đĩa.

Cột `snapshots.preview` chứa những gì đọc được từ các format dễ (JSON, INI,
XML) để UI hiển thị. Nó **không bao giờ** tham gia vào việc khôi phục — xem
[`preview.rs`](src-tauri/src/preview.rs).

### 4. Nén cực hạn khi lên cloud

Đo trên save thật, **không có thuật toán nào thắng mọi file**, nên mỗi file
được thử song song rồi giữ cách nhỏ nhất ([`pack.rs`](src-tauri/src/pack.rs)):

- Độc lập: `xz -9e`, `brotli-11`, `bzip2 -9`, `zstd-22`, hoặc lưu thô cho file
  không nén được (save đã mã hoá).
- **Delta** zstd-22 (kiểu `--patch-from`) so với phiên bản trước của chính file
  đó, **và** so với file anh em lớn nhất trong cùng snapshot.

Kết quả được giải ngược và so từng byte trước khi chấp nhận. Chuỗi delta tối
đa 8 mắt; dọn rác trên server hiểu quan hệ delta nên không bao giờ xoá một bản
còn là tổ tiên của bản đang dùng.

Đo trên save thật của máy phát triển:

| | Cách cũ (zstd-3) | Cực hạn |
|---|---|---|
| Stardew Valley, lần đẩy đầu (7 file, 11 MB) | 361,8 KB | **103,3 KB** |
| Stardew, mỗi lần đẩy sau (delta so với hôm trước) | ~133 KB | **~3,4 KB** |
| 3 phiên bản Stardew liên tiếp (11,2 MB) | — | **69,8 KB** (0,62%) |
| DAVE THE DIVER (2,3 MB) | 848,8 KB | **338,7 KB** |
| Stellar Blade (1,5 MB) | 51,0 KB | **33,7 KB** |

**Giới hạn thật:** save đã mã hoá (ELDEN RING NIGHTREIGN, phần lớn ELDEN RING)
không nén được bằng bất kỳ thuật toán nào — dữ liệu mã hoá không phân biệt được
với dữ liệu ngẫu nhiên. Với loại này chỉ còn dedupe và delta giữa các phiên bản.

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
cargo test                                                    # 53 unit test + watcher_real
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

### Test end-to-end thật (phá huỷ)

Hai file test này **xoá save thật rồi khôi phục từ cloud**:

```powershell
$env:CS_TEST_EMAIL="..."; $env:CS_TEST_PASSWORD="..."
cargo test --release --test supabase_e2e   -- --ignored --nocapture --test-threads=1
cargo test --release --test real_saves_e2e -- --ignored --nocapture --test-threads=1
```

[`real_saves_e2e.rs`](src-tauri/tests/real_saves_e2e.rs) lấy **mọi** save trên
mọi tài khoản Steam, đẩy lên cloud, xoá khỏi đĩa, rồi lấy về và so từng byte. Nó
chép mọi file sang chỗ riêng trước khi bắt đầu, và khi kết thúc — kể cả panic —
tự chép lại file nào thiếu hay lệch. Kết quả trên máy phát triển:

```
[1] 14 bộ save, 38 file, 65.856.241 byte
[3] đẩy lên cloud: 65,9 MB gốc → 22,2 MB lưu (21,5 MB trong đó là 2 save mã hoá)
[4] đã XOÁ 38 file save thật khỏi đĩa
[5] lấy về từ cloud: 38/38 file khớp từng byte với bản gốc
[lưới an toàn] 38 file được bảo vệ, 0 file phải chép lại từ bản sao
```

`delta_chain_survives_gc` đẩy 3 phiên bản thật của Stardew thành chuỗi delta,
xoá snapshot của 2 bản đầu, dọn rác, rồi khôi phục bản thứ ba — khẳng định dọn
rác không làm gãy chuỗi.

## Cấu trúc

```
src-tauri/src/
├─ steam/
│  ├─ appinfo.rs      parser binary VDF (magic 0x07564429, có string table)
│  ├─ remotecache.rs  parser text VDF + kiểm tra path traversal
│  ├─ locate.rs       dò Steam root, thư viện, tài khoản
│  ├─ cloudcfg.rs     đọc / sửa công tắc CloudEnabled trong sharedconfig.vdf
│  ├─ autocloud.rs    steam_autocloud.vdf + thư mục `ac` (Steam dời save đi đâu)
│  ├─ roots.rs        root token → đường dẫn tuyệt đối
│  └─ textvdf.rs      parser KeyValues dạng text
├─ manifest/ludusavi.rs   tải + cache manifest, map placeholder → root token
├─ scan.rs            gộp ba nguồn thành danh sách file, dedupe
├─ local_store.rs     kho trên máy: blob nén theo sha256 + snapshot JSON
├─ watcher.rs         game đang chạy (registry + tiến trình), chụp khi tắt,
│                     theo dõi tài khoản Steam và Steam Cloud
├─ steam_cloud.rs     trạng thái Steam Cloud, ghi sẵn cho tài khoản khác
├─ steam_ui.rs        tắt Steam Cloud qua giao diện Steam (OCR + chuột)
├─ backgrounds.rs     ảnh nền người dùng tự thêm
├─ web_backgrounds.rs ảnh nền xoay vòng từ Wikimedia Commons
├─ applog.rs          log ra %LOCALAPPDATA%\cloudsave-g4market\app.log
├─ pack.rs            nén cực hạn: xz/brotli/bzip2/zstd/delta, giữ cái nhỏ nhất
├─ remote_blob.rs     upload lát cắt, tải về + giải ngược chuỗi delta
├─ blob.rs            sha256 + chunk, định dạng cũ
├─ preview.rs         trích data để hiển thị (không dùng khi khôi phục)
├─ supabase.rs        GoTrue + PostgREST + RPC bytea
├─ backup.rs          đẩy một bản LOCAL: dedupe → upload → metadata → complete
├─ restore.rs         từ local hoặc cloud: verify → cứu hộ → tmp-then-rename
└─ commands.rs        lệnh Tauri

src-tauri/tests/
├─ scan_real_steam.rs integration test chạy trên Steam thật (#[ignore])
├─ watcher_real.rs    phát hiện tiến trình game thật
├─ supabase_e2e.rs    local → cloud → xoá đĩa → khôi phục, chuỗi delta + gc
└─ real_saves_e2e.rs  MỌI save thật → cloud → xoá → lấy về (#[ignore], phá huỷ)
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

## Ảnh nền

Chọn trong Cài đặt → Hình nền:

- **Hẻm núi** — một ảnh đứng yên.
- **Xoay vòng ảnh phong cảnh từ web** — đổi ảnh 45 giây một lần, lấy liên tục
  từ các danh mục ảnh tuyển chọn về phong cảnh và núi trên
  [Wikimedia Commons](https://commons.wikimedia.org) (không giới hạn). Góc dưới
  bên trái ghi tác giả và giấy phép của ảnh đang hiện (đa số CC BY-SA), bấm vào
  mở trang gốc. Mất mạng thì xoay ảnh có sẵn.
- **Xoay vòng ảnh của bạn** — ảnh tự thêm bằng nút "Thêm ảnh…" (chép vào
  `%LOCALAPPDATA%\cloudsave-g4market\backgrounds\`).

Thanh "Độ trong suốt" chỉnh độ mờ của các khung kính để thấy rõ nền.

Ảnh có sẵn (nhúng trong `src/assets/`) đều từ Unsplash, dùng theo
[giấy phép Unsplash](https://unsplash.com/license):

| Ảnh | Tác giả |
|---|---|
| [Mesa Arch, Canyonlands](https://unsplash.com/photos/mesa-arch-canyonlands-national-park-aZBpvUNWbsg) (mặc định) | [Ronald Diel](https://unsplash.com/@rondiel) |
| [Núi tuyết dưới trời sao, Dolomites](https://unsplash.com/photos/snowy-mountains-under-a-starry-night-sky-SumjjLhysZM) | Marek Piwnicki |
| [Đỉnh núi tuyết dưới trời sao](https://unsplash.com/photos/snowy-mountain-peak-under-a-starry-night-sky-txXfF_2YZpY) | Ahmet Yüksek |
| [Núi dưới trời sao, Yosemite](https://unsplash.com/photos/mountain-under-starry-sky-_vPbUVNk4Kc) | [Sam Goodgame](https://unsplash.com/@sgoodgame) |
| [Trời sao trên dãy núi](https://unsplash.com/photos/starry-night-sky-over-majestic-mountains-YPcwPGfX0yM) | [Ryan Klaus](https://unsplash.com/@ryankphoto) |
