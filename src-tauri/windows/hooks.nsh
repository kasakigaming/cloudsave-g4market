; Chuyển từ tên cũ "CloudSave G4Market" sang "SaveKeeper" (từ bản 0.1.8).
;
; Bộ cài của Tauri nhận ra bản đã cài theo TÊN SẢN PHẨM: khoá registry, thư mục
; cài và tên shortcut đều lấy từ tên đó. Đổi tên mà không làm gì thêm thì bản
; mới được cài *bên cạnh* bản cũ: shortcut cũ vẫn mở bản cũ, và danh sách
; chương trình của Windows có cả hai. Tệ nhất là khi app tự cập nhật: bộ cài
; chạy ở chế độ /UPDATE, chế độ đó không tạo shortcut nào — người dùng còn lại
; đúng cái shortcut trỏ vào bản cũ.
;
; Hai hook dưới đây dọn bản cũ. Chúng chỉ đụng tới đúng những thứ bộ cài cũ đã
; tạo, và chỉ khi registry còn ghi bản cũ:
;   - file exe và uninstall.exe trong thư mục cài cũ (không xoá đệ quy);
;   - shortcut Start menu / Desktop, chỉ khi nó trỏ vào exe cũ — và tạo lại
;     shortcut tên mới ở đúng chỗ đó;
;   - hai khoá registry của bản cũ.
;
; Dữ liệu của người dùng không nằm trong các chỗ này và không bị đụng tới: kho
; bản lưu ở %LOCALAPPDATA%\cloudsave-g4market, dữ liệu WebView theo identifier
; com.g4market.cloudsave — cả hai giữ nguyên tên.

!define OLD_PRODUCTNAME "CloudSave G4Market"
!define OLD_MAINBINARY "cloudsave-g4market.exe"
!define OLD_UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${OLD_PRODUCTNAME}"
; "g4market" là nhà phát hành mà bộ cài suy ra từ identifier; viết thẳng ra vì
; file này được nạp trước khi kịch bản chính định nghĩa MANUKEY.
!define OLD_MANUPRODUCTKEY "Software\g4market\${OLD_PRODUCTNAME}"

; Thay một shortcut của bản cũ bằng shortcut của bản mới, ở cùng thư mục.
; Chỉ làm khi shortcut cũ thật sự trỏ vào exe cũ: người dùng có thể đã tự tạo
; một file trùng tên trỏ đi nơi khác.
!macro MigrateOldShortcut dir
  !insertmacro IsShortcutTarget "${dir}\${OLD_PRODUCTNAME}.lnk" "$R7\${OLD_MAINBINARY}"
  Pop $0
  ${If} $0 = 1
    !insertmacro UnpinShortcut "${dir}\${OLD_PRODUCTNAME}.lnk"
    Delete "${dir}\${OLD_PRODUCTNAME}.lnk"
    ${IfNot} ${FileExists} "${dir}\${PRODUCTNAME}.lnk"
      CreateShortcut "${dir}\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
      !insertmacro SetLnkAppUserModelId "${dir}\${PRODUCTNAME}.lnk"
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  ; Bản cũ còn đang chạy thì phải đóng: exe của nó sắp bị gỡ, và hai bản chạy
  ; song song sẽ cùng ghi vào một kho bản lưu.
  ReadRegStr $R7 SHCTX "${OLD_MANUPRODUCTKEY}" ""
  ${If} $R7 != ""
    !insertmacro CheckIfAppIsRunning "${OLD_MAINBINARY}" "${OLD_PRODUCTNAME}"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ReadRegStr $R7 SHCTX "${OLD_MANUPRODUCTKEY}" ""
  ${If} $R7 != ""
  ${AndIf} $R7 != $INSTDIR
    !insertmacro MigrateOldShortcut "$SMPROGRAMS"
    !insertmacro MigrateOldShortcut "$DESKTOP"

    Delete "$R7\${OLD_MAINBINARY}"
    Delete "$R7\uninstall.exe"
    ; Không /r: thư mục chỉ bị gỡ khi đã trống.
    RMDir "$R7"

    DeleteRegKey SHCTX "${OLD_UNINSTKEY}"
    DeleteRegKey SHCTX "${OLD_MANUPRODUCTKEY}"
  ${EndIf}
!macroend
