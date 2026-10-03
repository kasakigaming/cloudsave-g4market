// Chọn theme TRƯỚC khi trang vẽ, để không chớp sai theme lúc mở. Script thường
// (không phải module) trong <head> chạy ngay, trước khi phần body được dựng.
// Giá trị hợp lệ: "flat" (mặc định) | "glass" (giao diện 2, kính).
(function () {
  var t = "flat";
  try {
    if (localStorage.getItem("cloudsave.theme") === "glass") t = "glass";
  } catch (e) {
    /* không đọc được bộ nhớ trình duyệt: dùng mặc định */
  }
  document.documentElement.dataset.theme = t;
})();
